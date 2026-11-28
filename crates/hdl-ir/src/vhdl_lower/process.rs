//! Process and concurrent-assignment lowering: processes and their variables, sensitivity lists, concurrent asserts, waveform elements, and selected/conditional assignment.

use super::*;

impl<'a> Lower<'a> {
    pub(super) fn lower_process(
        &mut self,
        module: &mut ModuleIr,
        p: &SyntaxNode,
        idx: usize,
    ) -> ProcessIr {
        let kids = real_children(p);
        let mut sens = Sens::List(Vec::new());
        let mut stmt_nodes: Vec<SyntaxNode> = Vec::new();
        let mut local_fns: Vec<SyntaxNode> = Vec::new();
        for k in &kids {
            match k.kind() {
                SyntaxKind::SensList => {
                    sens = self.lower_sens_list(k);
                }
                SyntaxKind::Function if self.subprogram_is_function(k) => {
                    if let Some(stub) = self.subprogram_stub(k) {
                        self.register_func(stub);
                    }
                    local_fns.push(k.clone());
                }
                SyntaxKind::SignalDecl
                | SyntaxKind::VariableDecl
                | SyntaxKind::ConstantDecl
                | SyntaxKind::TypeDecl
                | SyntaxKind::SubtypeDecl
                | SyntaxKind::AliasDecl
                | SyntaxKind::AttrDecl => {
                    // Process-local declarations.
                    match k.kind() {
                        SyntaxKind::VariableDecl => {
                            self.lower_process_var(module, k, idx);
                        }
                        SyntaxKind::ConstantDecl => {
                            self.lower_constant_decl(k);
                            self.materialize_const(module, k);
                        }
                        SyntaxKind::TypeDecl => self.lower_type_decl(k),
                        SyntaxKind::SubtypeDecl => self.lower_subtype_decl(k),
                        SyntaxKind::AliasDecl => self.lower_alias_decl(k),
                        SyntaxKind::AttrDecl => {
                            self.register_attr_decl(k);
                        }
                        _ => {}
                    }
                }
                SyntaxKind::UseClause => self.merge_use_clause(k),
                SyntaxKind::PackageInst => self.register_package_inst(k),
                SyntaxKind::FileDecl => self.lower_file_decl(k),
                _ if is_stmt_node(k.kind()) => stmt_nodes.push(k.clone()),
                _ => {}
            }
        }
        // Process-local functions (`function prefix return string is ...`):
        // register them so bare parameterless calls (`prefix & "..."`) resolve.
        for k in &local_fns {
            if let Some(fi) = self.lower_function_node(k) {
                self.register_func(fi);
            }
        }
        self.extra_base = module.signals.len();
        self.extra_signals.clear();
        let body = if stmt_nodes.len() == 1 {
            self.lower_seq_stmt(&stmt_nodes[0])
        } else {
            Stmt::Block(stmt_nodes.iter().map(|k| self.lower_seq_stmt(k)).collect())
        };
        for sig in self.extra_signals.drain(..) {
            module.signals.push(sig);
        }
        // VHDL processes always loop: with a sensitivity list they re-run on
        // each event (`always @(sens)`); without one they re-run from the top
        // whenever they reach the end, suspending only at wait statements
        // (empty sens list = bare `always`, re-queued immediately).
        let kind = ProcKind::Always;
        // Name the process by source location in diagnostics — an AI reading
        // the transcript can jump straight to the source. (The parser drops
        // `label :` before building the node, so labels don't reach the CST.)
        let sp = p.span();
        let origin = {
            let f = self.db.file(sp.file);
            format!(
                "process #{} at {}:{}",
                idx,
                f.name(),
                f.line_col(sp.start).0
            )
        };
        ProcessIr {
            kind,
            sens,
            body,
            origin,
            // VHDL processes execute once at t=0 (LRM 11.3), unlike a
            // Verilog `always @(...)` which waits for the first event.
            initial: true,
        }
    }

    pub(super) fn lower_sens_list(&mut self, s: &SyntaxNode) -> Sens {
        let mut edges = Vec::new();
        for c in real_children(s) {
            // The parser wraps postfix indexing (`arr(i)`, `data(7 downto 0)`)
            // in a Call/PartSelect node with the base Name nested first, so a
            // bare `SyntaxKind::Name` check silently drops indexed entries —
            // leaving the process with an empty sensitivity list, which the
            // sim treats as a bare `always` (infinite zero-delay re-run).
            // LRM 1076.4 puts the longest static prefix of each name in the
            // sensitivity set: for `arr(i)` that is `arr` itself.
            let name_node = match c.kind() {
                SyntaxKind::Name => Some(c.clone()),
                SyntaxKind::Call | SyntaxKind::PartSelect => real_children(&c)
                    .into_iter()
                    .find(|k| k.kind() == SyntaxKind::Name),
                _ => None,
            };
            let Some(name_node) = name_node else { continue };
            let name = self.text(&name_node);
            {
                // VHDL-2008 `process(all)`: sensitive to every signal read
                // (case-insensitive — vendor code writes `PROCESS (ALL)`).
                if name.eq_ignore_ascii_case("all") {
                    return Sens::All;
                }
                // Selected names (`bus_req_i.addr`, `arr(i).stb`) — sensitive to
                // the whole base signal.
                let base_name = name
                    .split(['.', '('])
                    .next()
                    .unwrap_or(name.as_str())
                    .to_string();
                let lookup = if base_name != name { &base_name } else { &name };
                if let Some(id) = self.sig_id(lookup) {
                    edges.push(Edge {
                        sig: id,
                        kind: EdgeKind::Any,
                    });
                } else if self
                    .pkg_sig_types
                    .contains_key(&lookup.to_ascii_lowercase())
                {
                    // Package-level signal (e.g. `signal runner` in a package
                    // declaration, used in `wait on runner`): placeholder.
                    let id = self.ensure_global_sig(lookup);
                    edges.push(Edge {
                        sig: id,
                        kind: EdgeKind::Any,
                    });
                } else {
                    self.err(
                        name_node.span(),
                        format!("unknown signal '{}' in sensitivity list", name),
                    );
                }
            }
        }
        Sens::List(edges)
    }

    /// Concurrent `assert cond report msg severity lvl;` → an always-process
    /// sensitive to the condition's signals, with the sequential assert as
    /// body. Runs once at t=0 like any VHDL process.
    pub(super) fn lower_conc_assert(&mut self, s: &SyntaxNode, idx: usize) -> ProcessIr {
        let body = self.lower_seq_stmt_impl(s);
        let mut sigs = Vec::new();
        if let Stmt::SysCall { args, .. } = &body {
            for a in args.iter().take(1) {
                a.collect_sigs(&mut sigs);
            }
        }
        // A condition without signals gives an empty sensitivity set: the
        // equivalent passive process runs once at initialization and never
        // re-fires. Lowering it as `Always` would make the kernel treat it
        // like a bare Verilog `always` and re-activate it every delta cycle
        // (generic-check asserts such as
        // `ASSERT (is_power_of_two(G_SIZE)) ...` would loop).
        let kind = if sigs.is_empty() {
            ProcKind::Initial
        } else {
            ProcKind::Always
        };
        ProcessIr {
            kind,
            sens: Sens::List(
                sigs.into_iter()
                    .map(|sig| Edge {
                        sig,
                        kind: EdgeKind::Any,
                    })
                    .collect(),
            ),
            body,
            origin: format!("concurrent assert #{idx}"),
            initial: true,
        }
    }

    /// `unaffected` waveform element (a bare keyword child of the assignment
    /// statement, not an expression node).
    pub(super) fn is_unaffected_val(&self, n: &SyntaxNode) -> bool {
        n.kind() == SyntaxKind::Ident && self.kw(n) == "unaffected"
    }

    /// One waveform element: `expr [after t] [when c]`.
    pub(super) fn waveform_elems(&self, kids: &[SyntaxNode], start: usize) -> Vec<WaveElem> {
        // Waveform elements: `expr [after t] [when c]`, comma-separated, or the
        // conditional form `expr when c else expr [when c]... [else expr]`.
        let mut elems: Vec<WaveElem> = Vec::new();
        let mut cur: Option<WaveElem> = None;
        let mut i = start;
        while i < kids.len() {
            let k = &kids[i];
            match self.kw(k).as_str() {
                "after" | "when" => {
                    if let Some(e) = kids.get(i + 1).filter(|e| is_expr_node(e.kind())) {
                        if let Some(c) = cur.as_mut() {
                            if self.kw(k) == "after" {
                                c.delay = Some(e.clone());
                            } else {
                                c.cond = Some(e.clone());
                            }
                        }
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                "else" => {
                    if let Some(c) = cur.take() {
                        elems.push(c);
                    }
                    if let Some(e) = kids.get(i + 1).filter(|e| is_expr_node(e.kind())) {
                        cur = Some(WaveElem {
                            val: e.clone(),
                            delay: None,
                            cond: None,
                        });
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                "unaffected" if k.kind() == SyntaxKind::Ident => {
                    if let Some(c) = cur.take() {
                        elems.push(c);
                    }
                    cur = Some(WaveElem {
                        val: k.clone(),
                        delay: None,
                        cond: None,
                    });
                    i += 1;
                }
                _ if is_expr_node(k.kind()) => {
                    if let Some(c) = cur.take() {
                        elems.push(c);
                    }
                    cur = Some(WaveElem {
                        val: k.clone(),
                        delay: None,
                        cond: None,
                    });
                    i += 1;
                }
                _ => i += 1,
            }
        }
        if let Some(c) = cur {
            elems.push(c);
        }
        elems
    }

    pub(super) fn lower_conc_assign(&mut self, s: &SyntaxNode, idx: usize) -> Option<ConcOut> {
        let kids = real_children(s);
        let lhs_pos = kids.iter().position(|k| {
            k.kind() == SyntaxKind::Name
                || k.kind() == SyntaxKind::Call
                || k.kind() == SyntaxKind::PartSelect
        })?;
        let lhs = self.lower_expr(&kids[lhs_pos], &self.consts.clone());
        let elems = self.waveform_elems(&kids, lhs_pos + 1);
        if elems.is_empty() {
            return None;
        }
        let has_cond = elems.iter().any(|e| e.cond.is_some());
        let has_delay = elems.iter().any(|e| e.delay.is_some());
        if has_delay {
            // VHDL waveform: each element's delay is relative to the time the
            // statement executes. `v after t` schedules a transaction at
            // now + t WITHOUT suspending the process (`AssignDelayed`), so the
            // equivalent process (LRM 11.3)
            //   `process begin <assigns>; wait on <rhs signals>; end process;`
            // re-arms on every RHS event — self-oscillators
            // (`clk <= not clk after 10 ns;`) keep ticking.
            let mut stmts = Vec::new();
            let mut rhs_sigs: Vec<SigId> = Vec::new();
            for e in &elems {
                // `unaffected` element: schedules no transaction.
                if self.is_unaffected_val(&e.val) {
                    continue;
                }
                let mut val = self.lower_rhs(&kids[lhs_pos], &e.val, &self.consts.clone());
                if let Some(c) = &e.cond {
                    // `v after t when c` — fold the condition into the value.
                    val = Expr::Cond {
                        c: Box::new(self.lower_expr(c, &self.consts.clone())),
                        t: Box::new(val),
                        e: Box::new(Expr::Lit(Value::x(1))),
                    };
                }
                val.collect_sigs(&mut rhs_sigs);
                let t_fs = match &e.delay {
                    Some(d) => {
                        let dv = self.lower_expr(d, &self.consts.clone());
                        match self.ce(&dv) {
                            Some(v) => v.to_u64().unwrap_or(0), // X (unoverridden generic) -> 0
                            None => {
                                // Non-constant delay (e.g. `after Baud/2` with Baud a
                                // signal): not schedulable exactly — apply the
                                // transaction with no delay instead of dropping the
                                // whole assignment.
                                self.warn(d.span(), "non-constant waveform delay; treated as 0");
                                0
                            }
                        }
                    }
                    None => 0,
                };
                stmts.push(Stmt::AssignDelayed {
                    lhs: lhs.clone(),
                    rhs: val,
                    delay: Expr::Lit(Value::from_u64(64, t_fs)),
                });
            }
            if stmts.is_empty() {
                return None; // every element `unaffected`: no assignment
            }
            let body = if stmts.len() == 1 {
                stmts.pop().expect("checked stmts.len() == 1")
            } else {
                Stmt::Block(stmts)
            };
            if rhs_sigs.is_empty() {
                // A waveform reading no signal: schedule once, no wake source.
                return Some(ConcOut::Proc(ProcessIr {
                    kind: ProcKind::Initial,
                    sens: Sens::List(Vec::new()),
                    body,
                    origin: format!("concurrent assign #{}", idx),
                    initial: false,
                }));
            }
            // Loop with a trailing `wait on <rhs signals>` so each RHS event
            // re-executes the assignment. The explicit Wait parks in
            // `self.waiting`; no implicit sens re-park (which would miss the
            // process's own writes).
            let wake = Stmt::Wait {
                sens: rhs_sigs
                    .iter()
                    .map(|s| Edge {
                        sig: *s,
                        kind: EdgeKind::Any,
                    })
                    .collect(),
                body: Box::new(Stmt::Null),
            };
            let body = Stmt::For {
                init: Box::new(Stmt::Null),
                cond: Expr::Lit(Value::from_u64(1, 1)),
                step: Box::new(Stmt::Null),
                body: Box::new(Stmt::Block(vec![body, wake])),
            };
            return Some(ConcOut::Proc(ProcessIr {
                kind: ProcKind::Initial,
                sens: Sens::List(Vec::new()),
                body,
                origin: format!("concurrent assign #{}", idx),
                initial: false,
            }));
        }
        if has_cond {
            // Conditional waveform → nested ternary, first true condition wins.
            let mut rhs: Option<Expr> = None;
            for e in elems.iter().rev() {
                // A trailing `else unaffected` keeps the current value:
                // lower it as a self-assignment (no event).
                let v = if self.is_unaffected_val(&e.val) {
                    lhs.clone()
                } else {
                    self.lower_rhs(&kids[lhs_pos], &e.val, &self.consts.clone())
                };
                rhs = Some(match (&e.cond, rhs) {
                    (Some(c), Some(r)) => Expr::Cond {
                        c: Box::new(self.lower_expr(c, &self.consts.clone())),
                        t: Box::new(v),
                        e: Box::new(r),
                    },
                    (Some(c), None) => Expr::Cond {
                        c: Box::new(self.lower_expr(c, &self.consts.clone())),
                        t: Box::new(v),
                        e: Box::new(Expr::Lit(Value::x(1))),
                    },
                    (None, r) => r.unwrap_or(v),
                });
            }
            return Some(ConcOut::Assign(AssignIr { lhs, rhs: rhs? }));
        }
        // Plain waveform: the last element at t=0 wins (`unaffected`
        // elements add no transaction and are dropped).
        let elems: Vec<WaveElem> = elems
            .into_iter()
            .filter(|e| !self.is_unaffected_val(&e.val))
            .collect();
        let rhs = self.lower_rhs(&kids[lhs_pos], &elems.last()?.val, &self.consts.clone());
        Some(ConcOut::Assign(AssignIr { lhs, rhs }))
    }

    /// `with sel select target <= w1 when c1, w2 when c2, ...;` → a continuous
    /// assign of nested ternaries (first matching choice wins, `others` last).
    pub(super) fn lower_selected_assign(&mut self, s: &SyntaxNode) -> Option<AssignIr> {
        let kids = real_children(s);
        let sel_pos = kids.iter().position(|k| is_expr_node(k.kind()))?;
        let target_pos = kids[sel_pos + 1..]
            .iter()
            .position(|k| k.kind() == SyntaxKind::Name)?
            + sel_pos
            + 1;
        let sel = self.lower_expr(&kids[sel_pos], &self.consts.clone());
        let lhs = self.lower_expr(&kids[target_pos], &self.consts.clone());
        // Walk `value [after t] when (choice | ... | others) [, ...]` pairs.
        let mut pairs: Vec<(SyntaxNode, Vec<SyntaxNode>, bool)> = Vec::new();
        let mut cur_val: Option<SyntaxNode> = None;
        let mut cur_choices: Vec<SyntaxNode> = Vec::new();
        let mut cur_others = false;
        let mut in_choices = false;
        for k in &kids[target_pos + 1..] {
            match self.kw(k).as_str() {
                "when" => in_choices = true,
                "others" => cur_others = true,
                _ if k.kind() == SyntaxKind::Punct && self.text(k) == "," => {
                    if let Some(v) = cur_val.take() {
                        pairs.push((v, std::mem::take(&mut cur_choices), cur_others));
                        cur_others = false;
                    }
                    in_choices = false;
                }
                _ if is_expr_node(k.kind()) => {
                    if in_choices {
                        cur_choices.push(k.clone());
                    } else {
                        cur_val = Some(k.clone());
                    }
                }
                _ => {}
            }
        }
        if let Some(v) = cur_val {
            pairs.push((v, cur_choices, cur_others));
        }
        if pairs.is_empty() {
            return None;
        }
        let mut rhs: Option<Expr> = None;
        for (val, choices, others) in pairs.iter().rev() {
            let v = self.lower_expr(val, &self.consts.clone());
            if *others {
                rhs = Some(rhs.unwrap_or(v));
                continue;
            }
            let mut cond: Option<Expr> = None;
            for c in choices {
                let e = Expr::Bin {
                    op: BinOp::Eq,
                    l: Box::new(sel.clone()),
                    r: Box::new(self.lower_expr(c, &self.consts.clone())),
                };
                cond = Some(match cond {
                    None => e,
                    Some(prev) => Expr::Bin {
                        op: BinOp::LogOr,
                        l: Box::new(prev),
                        r: Box::new(e),
                    },
                });
            }
            let else_e = rhs.unwrap_or_else(|| Expr::Lit(Value::x(1)));
            rhs = Some(match cond {
                Some(c) => Expr::Cond {
                    c: Box::new(c),
                    t: Box::new(v),
                    e: Box::new(else_e),
                },
                None => else_e,
            });
        }
        Some(AssignIr { lhs, rhs: rhs? })
    }
}
