//! Sequential-statement lowering: if/case/loops/waits, protected-type LHS helpers, and flat-slot loop ranges.
//!
//! Extracted verbatim from vhdl_lower.rs (pure move, no logic changes).

use super::*;

impl<'a> Lower<'a> {
    /// Lower a run of sequential statements into a Block.
    pub(super) fn lower_seq_block(&mut self, n: &SyntaxNode) -> Stmt {
        let stmts: Vec<Stmt> = real_children(n)
            .iter()
            .filter(|k| is_stmt_node(k.kind()))
            .map(|k| self.lower_seq_stmt(k))
            .collect();
        if stmts.len() == 1 {
            stmts.into_iter().next().expect("checked stmts.len() == 1")
        } else {
            Stmt::Block(stmts)
        }
    }

    /// Lower one sequential statement, wrapped in a `Stmt::Spanned` with its
    /// source span (drives line/branch coverage).
    pub(super) fn lower_seq_stmt(&mut self, s: &SyntaxNode) -> Stmt {
        let span = s.span();
        let inner = self.lower_seq_stmt_impl(s);
        Stmt::Spanned {
            span,
            inner: Box::new(inner),
        }
    }

    /// If `l` is a bare state-variable name of the protected body currently
    /// being lowered (and not shadowed by a local/param), return
    /// (type, field, lsb, width) so the assignment becomes a `$setf` write.
    pub(super) fn prot_field_lhs(&self, l: &SyntaxNode) -> Option<(String, String, usize, usize)> {
        if l.kind() != SyntaxKind::Name {
            return None;
        }
        let key = self.kw(l).to_ascii_lowercase();
        if self.sig_id(&key).is_some() {
            return None; // local/param shadows the field
        }
        let t = self.prot_body_ctx.as_ref()?;
        let pt = self.prot_types.get(t)?;
        let f = pt.fields.iter().find(|(n, _, _, _)| *n == key);
        let (_, w, lsb, _) = f?;
        Some((t.clone(), key, *lsb, *w))
    }

    /// Assignment target rooted at a protected-type state variable but not a
    /// whole field (`st.ptr := …`, `st.idxs(st.idx) := …` with `st` a state
    /// field): cannot use `$setf` — the statement is dropped (placeholder).
    pub(super) fn prot_nested_lhs(&self, l: &SyntaxNode) -> bool {
        let text = match l.kind() {
            SyntaxKind::Name => self.kw(l),
            SyntaxKind::Call => real_children(l)
                .iter()
                .find(|c| c.kind() == SyntaxKind::Name)
                .map(|c| self.kw(c))
                .unwrap_or_default(),
            _ => return false,
        };
        let base = text.split('.').next().unwrap_or("").to_ascii_lowercase();
        if base.is_empty() || self.sig_id(&base).is_some() {
            return false; // local/param shadows the field
        }
        let Some(t) = self.prot_body_ctx.as_ref() else {
            return false;
        };
        let Some(pt) = self.prot_types.get(t) else {
            return false;
        };
        pt.fields.iter().any(|(n, _, _, _)| *n == base)
    }

    /// Conditional expression tail shared by signal and variable assignment
    /// (`x := a when c else b;`): lower the conditional waveform over the
    /// `when`-clause tail and wrap it in the assignment.
    fn lower_cond_assign(
        &mut self,
        tail: &[SyntaxNode],
        l: &SyntaxNode,
        nba: bool,
    ) -> Option<Stmt> {
        self.lower_cond_waveform(tail, &self.consts.clone())
            .map(|e| Stmt::Assign {
                lhs: self.lower_expr(l, &self.consts.clone()),
                rhs: e,
                nba,
            })
    }

    /// Plain scalar assignment tail shared by signal (`<=`, delta-delayed)
    /// and variable (`:=`, blocking) statements: a protected-type field LHS
    /// lowers to a `$setf` field write, a nested protected LHS is skipped
    /// (unsupported), anything else is a plain assignment.
    fn finish_scalar_assign(&mut self, l: &SyntaxNode, r: &SyntaxNode, nba: bool) -> Stmt {
        if let Some((t, f, lsb, w)) = self.prot_field_lhs(l) {
            let rv = self.lower_rhs(l, r, &self.consts.clone());
            return Stmt::SysCall {
                name: format!("$setf::{}::{}@{},{}", t, f, lsb, w),
                args: vec![
                    Expr::Call {
                        name: "$this".into(),
                        args: vec![],
                        shapes: Vec::new(),
                    },
                    rv,
                ],
                shapes: Vec::new(),
            };
        }
        if self.prot_nested_lhs(l) {
            return Stmt::Null;
        }
        Stmt::Assign {
            lhs: self.lower_expr(l, &self.consts.clone()),
            rhs: self.lower_rhs(l, r, &self.consts.clone()),
            nba,
        }
    }

    /// Lower a sequential signal assignment `s <= ...;`: a waveform with
    /// per-element delays, a conditional waveform, or a plain delta-delayed
    /// assignment. A parenthesized target list is not lowered.
    fn lower_seq_assign(&mut self, s: &SyntaxNode) -> Stmt {
        let kids = real_children(s);
        // `(a, b) := f(...);` — parenthesized target list: not
        // lowered (no error).
        if kids.first().is_some_and(|k| k.kind() == SyntaxKind::Punct) {
            return Stmt::Null;
        }
        let lhs = kids.iter().find(|k| {
            matches!(
                k.kind(),
                SyntaxKind::Name | SyntaxKind::Call | SyntaxKind::PartSelect
            )
        });
        let op_pos = kids
            .iter()
            .position(|k| k.kind() == SyntaxKind::Punct && self.text(k) == "<");
        let has_when = kids
            .iter()
            .any(|k| k.kind() == SyntaxKind::Ident && self.kw(k) == "when");
        let has_after = kids
            .iter()
            .any(|k| k.kind() == SyntaxKind::Ident && self.kw(k) == "after");
        if has_after {
            return self.lower_waveform_assign(&kids, lhs, op_pos);
        }
        if has_when {
            if let (Some(l), Some(op)) = (lhs, op_pos) {
                if let Some(a) = self.lower_cond_assign(&kids[op + 2..], l, true) {
                    return a;
                }
            }
            return Stmt::Null;
        }
        let rhs = kids.iter().rev().find(|k| is_expr_node(k.kind()));
        match (lhs, rhs) {
            // VHDL signal assignment is delta-delayed
            (Some(l), Some(r)) => self.finish_scalar_assign(l, r, true),
            _ => Stmt::Null,
        }
    }

    /// Lower the waveform form of a signal assignment, `s <= v [after t]
    /// [, v2 [after t2] ...] [when c] ...`: each element is a transaction
    /// scheduled at now + its own delay (VHDL waveform delays are relative
    /// to the statement execution, not cumulative) — the process is not
    /// suspended.
    fn lower_waveform_assign(
        &mut self,
        kids: &[SyntaxNode],
        lhs: Option<&SyntaxNode>,
        op_pos: Option<usize>,
    ) -> Stmt {
        if let (Some(l), Some(op)) = (lhs, op_pos) {
            // `unaffected` elements schedule no transaction.
            let elems: Vec<WaveElem> = self
                .waveform_elems(kids, op + 1)
                .into_iter()
                .filter(|e| !self.is_unaffected_val(&e.val))
                .collect();
            if !elems.is_empty() {
                let lhs_e = self.lower_expr(l, &self.consts.clone());
                let mut stmts = Vec::new();
                for e in &elems {
                    // The delay may be a runtime expression (e.g.
                    // OSVVM `after iDelayVal`): evaluated when the
                    // statement executes.
                    let delay = e
                        .delay
                        .as_ref()
                        .map(|d| self.lower_expr(d, &self.consts.clone()));
                    let val = self.lower_rhs(l, &e.val, &self.consts.clone());
                    let a = match &delay {
                        None => Stmt::Assign {
                            lhs: lhs_e.clone(),
                            rhs: val,
                            nba: true,
                        },
                        Some(dv) => Stmt::AssignDelayed {
                            lhs: lhs_e.clone(),
                            rhs: val,
                            delay: dv.clone(),
                        },
                    };
                    stmts.push(match &e.cond {
                        Some(c) => Stmt::If {
                            cond: self.lower_expr(c, &self.consts.clone()),
                            then: Box::new(a),
                            els: None,
                        },
                        None => a,
                    });
                }
                return if stmts.len() == 1 {
                    stmts.pop().expect("checked stmts.len() == 1")
                } else {
                    Stmt::Block(stmts)
                };
            }
        }
        Stmt::Null
    }

    /// Lower a sequential variable assignment `v := ...;`: blocking (nba =
    /// false), either as a conditional expression or a plain assignment.
    fn lower_var_assign(&mut self, s: &SyntaxNode) -> Stmt {
        let kids = real_children(s);
        let lhs = kids.iter().find(|k| {
            k.kind() == SyntaxKind::Name
                || k.kind() == SyntaxKind::Call
                || k.kind() == SyntaxKind::PartSelect
        });
        // Conditional expression: `v := a when c else b;`
        let has_when = kids
            .iter()
            .any(|k| k.kind() == SyntaxKind::Ident && self.kw(k) == "when");
        if has_when {
            let colon_pos = kids
                .iter()
                .position(|k| k.kind() == SyntaxKind::Punct && self.text(k) == ":");
            if let (Some(l), Some(cp)) = (lhs, colon_pos) {
                if let Some(a) = self.lower_cond_assign(&kids[cp + 2..], l, false) {
                    return a;
                }
            }
            return Stmt::Null;
        }
        let rhs = kids.iter().rev().find(|k| is_expr_node(k.kind()));
        match (lhs, rhs) {
            (Some(l), Some(r)) => self.finish_scalar_assign(l, r, false),
            _ => Stmt::Null,
        }
    }

    pub(super) fn lower_seq_stmt_impl(&mut self, s: &SyntaxNode) -> Stmt {
        match s.kind() {
            SyntaxKind::SeqAssign => self.lower_seq_assign(s),
            SyntaxKind::VarAssign => self.lower_var_assign(s),
            SyntaxKind::If => self.lower_if(s),
            SyntaxKind::Case => self.lower_case(s),
            SyntaxKind::For => self.lower_for(s),
            SyntaxKind::LoopStmt => self.lower_loop(s),
            SyntaxKind::Wait => self.lower_wait(s),
            // Sequential block statement: lower its body statements as a
            // block (block-local declarations are not simulated).
            SyntaxKind::Block => self.lower_block_stmt(s),
            SyntaxKind::NullStmt => Stmt::Null,
            SyntaxKind::AssertStmt => self.lower_assert_stmt(s),
            SyntaxKind::ExitStmt => self.lower_exit_stmt(s),
            SyntaxKind::ReturnStmt => self.lower_return_stmt(s),
            SyntaxKind::CallStmt => self.lower_call_stmt(s),
            _ => Stmt::Null,
        }
    }

    /// Lower a sequential block statement: its body statements as a block
    /// (block-local declarations are not simulated).
    fn lower_block_stmt(&mut self, s: &SyntaxNode) -> Stmt {
        let stmts: Vec<Stmt> = real_children(s)
            .iter()
            .filter(|k| !is_decl_kind(k.kind()))
            .filter(|k| !matches!(k.kind(), SyntaxKind::Ident | SyntaxKind::Punct))
            .map(|k| self.lower_seq_stmt(k))
            .collect();
        Stmt::Block(stmts)
    }

    /// Lower an assert/report statement: covers both `assert cond [report
    /// msg] [severity lvl]` and `report msg [severity lvl]` (the parser
    /// uses one node kind).
    fn lower_assert_stmt(&mut self, s: &SyntaxNode) -> Stmt {
        let kids = real_children(s);
        let first_kw = kids
            .iter()
            .find(|k| k.kind() == SyntaxKind::Ident)
            .map(|k| self.kw(k))
            .unwrap_or_default();
        let severity = self.assert_severity(&kids, &first_kw);
        let mut exprs: Vec<Expr> = kids
            .iter()
            .filter(|k| is_expr_node(k.kind()))
            .map(|k| self.lower_expr(k, &self.consts.clone()))
            .collect();
        if !severity.is_empty() {
            exprs.push(Expr::Lit(string_value(&severity)));
        }
        Stmt::SysCall {
            name: if first_kw == "report" {
                "report"
            } else {
                "assert"
            }
            .to_string(),
            args: exprs,
            shapes: Vec::new(),
        }
    }

    /// Resolve an assert/report severity to its LRM name ("note" /
    /// "warning" / "error" / "failure") — empty when it cannot be known at
    /// lower time. A written keyword wins. A `severity C` named constant is
    /// const-folded: the parser keeps a non-keyword severity as a bare
    /// identifier (not an expression), enum constants carry their
    /// declaration-order position as the value (see types.rs), and
    /// severity_level is (note, warning, error, failure). Nothing written
    /// defaults per LRM 10.2 to error for assert (report's note default is
    /// applied by the kernel). A written non-constant (variable) severity
    /// stays empty rather than guessing a level.
    fn assert_severity(&self, kids: &[SyntaxNode], first_kw: &str) -> String {
        let keyword = kids
            .iter()
            .filter(|k| k.kind() == SyntaxKind::Ident)
            .map(|k| self.kw(k))
            .find(|w| matches!(w.as_str(), "note" | "warning" | "error" | "failure"));
        if let Some(w) = keyword {
            return w;
        }
        let written = kids
            .iter()
            .skip_while(|k| !(k.kind() == SyntaxKind::Ident && self.kw(k) == "severity"))
            .skip(1)
            .find(|k| k.kind() == SyntaxKind::Ident)
            .map(|k| self.kw(k));
        let Some(name) = written else {
            return if first_kw == "report" {
                String::new()
            } else {
                "error".to_string()
            };
        };
        let folded = self
            .consts
            .get(&name)
            .to_owned()
            .and_then(|v| match v.to_u64() {
                Some(0) => Some("note".to_string()),
                Some(1) => Some("warning".to_string()),
                Some(2) => Some("error".to_string()),
                Some(3) => Some("failure".to_string()),
                _ => None,
            });
        folded.unwrap_or_default()
    }

    /// Lower an `exit`/`next` statement. The parser folds both into one
    /// ExitStmt node: the first ident child is the keyword. `exit` ends the
    /// loop (break), `next` skips to the loop step (continue) — LRM 1076
    /// §10.11/§10.12. Both take an optional `when cond;`.
    fn lower_exit_stmt(&mut self, s: &SyntaxNode) -> Stmt {
        let kids = real_children(s);
        let is_next = kids
            .iter()
            .find(|k| k.kind() == SyntaxKind::Ident)
            .map(|k| self.kw(k).eq_ignore_ascii_case("next"))
            .unwrap_or(false);
        let jump = if is_next { Stmt::Continue } else { Stmt::Break };
        let cond = kids
            .iter()
            .find(|k| is_expr_node(k.kind()))
            .map(|k| self.lower_expr(k, &self.consts.clone()));
        match cond {
            Some(c) => Stmt::If {
                cond: c,
                then: Box::new(jump),
                els: None,
            },
            None => jump,
        }
    }

    /// Lower a return statement; a bare `return;` returns a zero-width
    /// placeholder value.
    fn lower_return_stmt(&mut self, s: &SyntaxNode) -> Stmt {
        let kids = real_children(s);
        let e = kids.iter().find(|k| is_expr_node(k.kind()));
        // A string-returning function's `RETURN "0"` (or any alphabet
        // literal) is text — lower the return expression in the string
        // context. `string'(...)` qualifiers set their own context.
        let saved = self.str_ctx;
        if self.fn_ret_str {
            self.str_ctx = true;
        }
        let ex = e
            .map(|e| self.lower_expr(e, &self.consts.clone()))
            .unwrap_or_else(|| Expr::Lit(Value::from_u64(1, 0)));
        self.str_ctx = saved;
        Stmt::Return(Box::new(ex))
    }

    /// Lower a procedure call statement `foo(a, b);` (or bare `foo;`):
    /// Textio builtins get a `$textio_` prefix, protected-type method calls
    /// dispatch to `$callmethod`, and `std.env.finish` lowers to
    /// `$env_finish` so the kernel can capture the STATUS argument;
    /// Verilog `$finish(n)` keeps the plain "finish" name (its argument is
    /// a diagnostic level, not an exit status). A user-defined procedure
    /// that happens to be named `finish` is treated the same way — the
    /// simulation still stops, just without a status.
    fn lower_call_stmt(&mut self, s: &SyntaxNode) -> Stmt {
        let kids = real_children(s);
        // `foo(a, b);` parses as CallStmt(Call(Name foo, (, args…)));
        // `foo;` as CallStmt(Name foo). Unwrap the Call so the callee
        // and the real arguments are found in both forms.
        let src: Vec<SyntaxNode> = match kids.first() {
            Some(c) if c.kind() == SyntaxKind::Call => real_children(c),
            _ => kids.clone(),
        };
        let name_pos = src.iter().position(|k| k.kind() == SyntaxKind::Name);
        let raw = name_pos.map(|i| self.text(&src[i])).unwrap_or_default();
        let fname = raw
            .rsplit('.')
            .next()
            .unwrap_or(raw.as_str())
            .to_ascii_lowercase();
        let name = if TEXTIO_PROCS.contains(&fname.as_str()) {
            format!("$textio_{fname}")
        } else {
            fname
        };
        let (args, arg_nodes) = self.call_stmt_args(&src, name_pos);
        if let Some(stmt) = self.prot_method_call(&raw, &name, &args) {
            return stmt;
        }
        let name = if name == "finish" {
            "$env_finish".to_string()
        } else {
            name
        };
        // A user procedure's shape-bound formals take the actuals' ranges:
        // bind what the actual's shape resolves to, omit the rest.
        let shapes = self.call_shape_binds(&name, &args, &arg_nodes);
        Stmt::SysCall { name, args, shapes }
    }

    /// Protected-type method dispatch for call statements: `obj.m(args);`
    /// on a protected object lowers to `$callmethod::t::m(obj, …)`, and a
    /// bare `m(args);` inside a protected-type body lowers to a self-call.
    /// `None` when the callee is not a method.
    fn prot_method_call(&mut self, raw: &str, name: &str, args: &[Expr]) -> Option<Stmt> {
        if let Some(dot) = raw.find('.') {
            let (base, m) = (&raw[..dot], raw[dot + 1..].to_ascii_lowercase());
            if let Some(id) = self.sig_id(base) {
                if let Some(t) = self.sig_prot.get(&id).cloned() {
                    let is_method = self
                        .prot_types
                        .get(&t)
                        .is_some_and(|p| p.methods.contains(&m));
                    if is_method {
                        let mut args2 = vec![Expr::Sig(id)];
                        args2.extend(args.iter().cloned());
                        return Some(Stmt::SysCall {
                            name: format!("$callmethod::{}::{}", t, m),
                            args: args2,
                            shapes: Vec::new(),
                        });
                    }
                }
            }
        }
        if let Some(t) = self.prot_body_ctx.clone() {
            if self
                .prot_types
                .get(&t)
                .is_some_and(|p| p.methods.iter().any(|m| m == name))
            {
                let mut args2 = vec![Expr::Call {
                    name: "$this".into(),
                    args: vec![],
                    shapes: Vec::new(),
                }];
                args2.extend(args.iter().cloned());
                return Some(Stmt::SysCall {
                    name: format!("$callmethod::{}::{}", t, name),
                    args: args2,
                    shapes: Vec::new(),
                });
            }
        }
        None
    }

    /// Collect a call statement's argument expressions. Association keys
    /// (`name => value`) are not arguments: drop a bare Name immediately
    /// followed by `=>`. Every other bare Name is a real positional
    /// argument — e.g. a variable passed to a user procedure — and must be
    /// kept; it may reference something from an unseen package/context, so
    /// it lowers leniently to a placeholder, like port-map association
    /// actuals.
    fn call_stmt_args(
        &mut self,
        src: &[SyntaxNode],
        name_pos: Option<usize>,
    ) -> (Vec<Expr>, Vec<SyntaxNode>) {
        let is_assoc_key: Vec<bool> = src
            .iter()
            .enumerate()
            .map(|(i, k)| {
                k.kind() == SyntaxKind::Name
                    && src
                        .get(i + 1)
                        .map(|n| self.text(n) == "=>")
                        .unwrap_or(false)
            })
            .collect();
        let nodes: Vec<SyntaxNode> = src
            .iter()
            .enumerate()
            .filter(|(i, k)| Some(*i) != name_pos && is_expr_node(k.kind()) && !is_assoc_key[*i])
            .map(|(_, k)| k.clone())
            .collect();
        let args = nodes
            .iter()
            .map(|k| {
                if k.kind() == SyntaxKind::Name {
                    self.suppress_err += 1;
                    let e = self.lower_expr(k, &self.consts.clone());
                    self.suppress_err -= 1;
                    e
                } else {
                    self.lower_expr(k, &self.consts.clone())
                }
            })
            .collect();
        (args, nodes)
    }

    pub(super) fn lower_if(&mut self, s: &SyntaxNode) -> Stmt {
        // Children: cond expr, then stmts..., then for each `elsif`: Ident,
        // cond expr, stmts...; an `else`: Ident + stmts....
        let kids = real_children(s);
        let mut branches: Vec<(Option<Expr>, Vec<&SyntaxNode>)> = Vec::new();
        let mut cur_cond: Option<Expr> = None;
        let mut cur_stmts: Vec<&SyntaxNode> = Vec::new();
        let mut pending_cond = false;
        for k in &kids {
            match k.kind() {
                SyntaxKind::Ident => {
                    let w = self.kw(k);
                    if (w == "elsif" || w == "else") && cur_cond.is_some() {
                        branches.push((cur_cond.take(), std::mem::take(&mut cur_stmts)));
                        pending_cond = w == "elsif";
                    }
                }
                _ if is_expr_node(k.kind()) && pending_cond => {
                    cur_cond = Some(self.lower_expr(k, &self.consts.clone()));
                    pending_cond = false;
                }
                _ if is_expr_node(k.kind()) && cur_cond.is_none() => {
                    cur_cond = Some(self.lower_expr(k, &self.consts.clone()));
                }
                _ if is_stmt_node(k.kind()) => cur_stmts.push(k),
                _ => {}
            }
        }
        branches.push((cur_cond, cur_stmts));
        // Fold into nested If from the inside out.
        let mut result = Stmt::Null;
        for (cond, stmts) in branches.into_iter().rev() {
            let body = {
                let ss: Vec<Stmt> = stmts.iter().map(|k| self.lower_seq_stmt(k)).collect();
                if ss.len() == 1 {
                    ss.into_iter().next().expect("checked ss.len() == 1")
                } else {
                    Stmt::Block(ss)
                }
            };
            result = Stmt::If {
                cond: cond.unwrap_or_else(|| Expr::Lit(Value::from_u64(1, 1))),
                then: Box::new(body),
                els: Some(Box::new(result)),
            };
        }
        result
    }

    pub(super) fn lower_case(&mut self, s: &SyntaxNode) -> Stmt {
        let kids = real_children(s);
        let matching = kids.iter().any(|k| k.kind() == SyntaxKind::Matching);
        let expr = kids.iter().find(|k| is_expr_node(k.kind()));
        let expr = expr
            .map(|k| self.lower_expr(k, &self.consts.clone()))
            .unwrap_or_else(|| Expr::Lit(Value::x(1)));
        let mut items: Vec<(Vec<Expr>, Stmt)> = Vec::new();
        let mut default = None;
        for item in kids.iter().filter(|k| k.kind() == SyntaxKind::CaseItem) {
            let ik = real_children(item);
            let is_others = ik.iter().any(|k| k.kind() == SyntaxKind::Others);
            let conds: Vec<Expr> = ik
                .iter()
                .filter(|k| is_expr_node(k.kind()) && k.kind() != SyntaxKind::Others)
                .map(|k| self.lower_expr(k, &self.consts.clone()))
                .collect();
            let body = self.lower_seq_block(item);
            if is_others {
                default = Some(Box::new(body));
            } else {
                items.push((conds, body));
            }
        }
        Stmt::Case {
            expr,
            items,
            default,
            matching,
        }
    }
}
