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
                    },
                    rv,
                ],
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

    pub(super) fn lower_seq_stmt_impl(&mut self, s: &SyntaxNode) -> Stmt {
        match s.kind() {
            SyntaxKind::SeqAssign => self.lower_seq_assign(s),
            SyntaxKind::VarAssign => {
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
            SyntaxKind::If => self.lower_if(s),
            SyntaxKind::Case => self.lower_case(s),
            SyntaxKind::For => self.lower_for(s),
            SyntaxKind::LoopStmt => self.lower_loop(s),
            SyntaxKind::Wait => self.lower_wait(s),
            // Sequential block statement: lower its body statements as a
            // block (block-local declarations are not simulated).
            SyntaxKind::Block => {
                let stmts: Vec<Stmt> = real_children(s)
                    .iter()
                    .filter(|k| !is_decl_kind(k.kind()))
                    .filter(|k| !matches!(k.kind(), SyntaxKind::Ident | SyntaxKind::Punct))
                    .map(|k| self.lower_seq_stmt(k))
                    .collect();
                Stmt::Block(stmts)
            }
            SyntaxKind::NullStmt => Stmt::Null,
            SyntaxKind::AssertStmt => {
                // Covers both `assert cond [report msg] [severity lvl]` and
                // `report msg [severity lvl]` (the parser uses one node kind).
                let kids = real_children(s);
                let first_kw = kids
                    .iter()
                    .find(|k| k.kind() == SyntaxKind::Ident)
                    .map(|k| self.kw(k))
                    .unwrap_or_default();
                let severity = kids
                    .iter()
                    .filter(|k| k.kind() == SyntaxKind::Ident)
                    .map(|k| self.kw(k))
                    .find(|w| matches!(w.as_str(), "note" | "warning" | "error" | "failure"))
                    .unwrap_or_default();
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
                }
            }
            SyntaxKind::ExitStmt => {
                // The parser folds `exit` and `next` into one ExitStmt node:
                // the first ident child is the keyword. `exit` ends the loop
                // (break), `next` skips to the loop step (continue) — LRM
                // 1076 §10.11/§10.12. Both take an optional `when cond;`.
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
            SyntaxKind::ReturnStmt => {
                let kids = real_children(s);
                let e = kids.iter().find(|k| is_expr_node(k.kind()));
                Stmt::Return(Box::new(
                    e.map(|e| self.lower_expr(e, &self.consts.clone()))
                        .unwrap_or_else(|| Expr::Lit(Value::from_u64(1, 0))),
                ))
            }
            SyntaxKind::CallStmt => {
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
                let is_textio = TEXTIO_PROCS.contains(&fname.as_str());
                // Association keys (`name => value`) are not arguments: drop
                // a bare Name immediately followed by `=>`. Every other bare
                // Name is a real positional argument — e.g. a variable passed
                // to a user procedure — and must be kept.
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
                let args: Vec<Expr> = src
                    .iter()
                    .enumerate()
                    .filter(|(i, k)| {
                        Some(*i) != name_pos && is_expr_node(k.kind()) && !is_assoc_key[*i]
                    })
                    .map(|(_, k)| {
                        if k.kind() == SyntaxKind::Name {
                            // A bare-name argument may reference something from
                            // an unseen package/context — lower leniently to a
                            // placeholder, like port-map association actuals.
                            self.suppress_err += 1;
                            let e = self.lower_expr(k, &self.consts.clone());
                            self.suppress_err -= 1;
                            e
                        } else {
                            self.lower_expr(k, &self.consts.clone())
                        }
                    })
                    .collect();
                let name = if is_textio {
                    format!("$textio_{fname}")
                } else {
                    fname
                };
                // `obj.m(args);` on a protected-type object →
                // `$callmethod::t::m(obj, …)`.
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
                                return Stmt::SysCall {
                                    name: format!("$callmethod::{}::{}", t, m),
                                    args: args2,
                                };
                            }
                        }
                    }
                }
                // Bare `m(args);` inside a protected-type body → self-call.
                if let Some(t) = self.prot_body_ctx.clone() {
                    if self
                        .prot_types
                        .get(&t)
                        .is_some_and(|p| p.methods.contains(&name))
                    {
                        let mut args2 = vec![Expr::Call {
                            name: "$this".into(),
                            args: vec![],
                        }];
                        args2.extend(args.iter().cloned());
                        return Stmt::SysCall {
                            name: format!("$callmethod::{}::{}", t, name),
                            args: args2,
                        };
                    }
                }
                // std.env.finish([status]) lowers to a distinct syscall so the
                // kernel can capture the STATUS argument; Verilog `$finish(n)`
                // keeps the plain "finish" name (its argument is a diagnostic
                // level, not an exit status). A user-defined procedure that
                // happens to be named `finish` is treated the same way — the
                // simulation still stops, just without a status.
                let name = if name == "finish" {
                    "$env_finish".to_string()
                } else {
                    name
                };
                Stmt::SysCall { name, args }
            }
            _ => Stmt::Null,
        }
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

    /// `name'range` bounds for a flat value slot (function formal, unsized
    /// signal): `0 to width-1`, resolved from the bound value at evaluation
    /// time via `SigAttr`. Only applies when the slot is not memory-backed
    /// and has no static dims — real array signals keep their static bounds.
    pub(super) fn flat_slot_range(&self, id: SigId) -> Option<(Expr, Expr)> {
        if self.mems.contains_key(&id) || self.sig_dims.contains_key(&id) {
            return None;
        }
        // The loop variable walks ELEMENTS, so the high bound is the bound
        // value's bit width divided by the element width, minus one — a
        // 64-bit-real formal's 'range yields count-1, not bit-1 (a bit-index
        // bound overran multi-bit-element formals by the element factor).
        // 1-bit elements keep the plain width-1 bound.
        let ew = self.sig_elem.get(&id).copied().unwrap_or(1).max(1) as u64;
        let count = if ew == 1 {
            Expr::SigAttr {
                sig: id,
                attr: "length",
            }
        } else {
            Expr::Bin {
                op: BinOp::Div,
                l: Box::new(Expr::SigAttr {
                    sig: id,
                    attr: "length",
                }),
                r: Box::new(Expr::Lit(Value::from_u64(32, ew))),
            }
        };
        Some((
            Expr::SigAttr {
                sig: id,
                attr: "low",
            },
            Expr::Bin {
                op: BinOp::Sub,
                l: Box::new(count),
                r: Box::new(Expr::Lit(Value::from_u64(32, 1))),
            },
        ))
    }

    pub(super) fn lower_for(&mut self, s: &SyntaxNode) -> Stmt {
        let kids = real_children(s);
        // for <var> in <range> loop <body> end loop
        let var = kids
            .iter()
            .find(|k| {
                k.kind() == SyntaxKind::Ident
                    && self.kw(k) != "for"
                    && self.kw(k) != "in"
                    && self.kw(k) != "loop"
                    && self.kw(k) != "end"
            })
            .map(|k| self.text(k));
        let range = kids
            .iter()
            .find(|k| k.kind() == SyntaxKind::Range || k.kind() == SyntaxKind::Attr);
        let Some(var) = var else {
            self.err(s.span(), "malformed for loop");
            return Stmt::Null;
        };
        // VHDL for-loop variables are implicitly declared by the loop.
        let var_id = match self.sig_id(&var) {
            Some(id) => id,
            None => {
                let id = (self.extra_base + self.extra_signals.len()) as SigId;
                self.extra_signals.push(SignalIr {
                    name: format!("__for_{}", var),
                    width: 32,
                    kind: SigKind::Wire,
                    init: None,
                    mem: None,
                    signed: false,
                });
                self.names.insert(var.clone(), id);
                id
            }
        };
        // Range bounds.
        let (lo, hi, downto) = match range.map(|r| r.kind()) {
            Some(SyntaxKind::Range) => {
                let rk = real_children(range.expect("matched on range's kind, so range is Some"));
                // `for i in s'range loop` — the range is a Range node holding an
                // Attr child; resolve the bounds from the signal's dims.
                if let Some(at) = rk.iter().find(|k| k.kind() == SyntaxKind::Attr) {
                    let ak = real_children(at);
                    let attr = ak
                        .iter()
                        .rfind(|k| k.kind() == SyntaxKind::Ident)
                        .map(|k| self.kw(k))
                        .unwrap_or_default();
                    if attr == "range" || attr == "reverse_range" {
                        if let Some(b) = ak.iter().find(|k| k.kind() == SyntaxKind::Name) {
                            let bn = self.text(b);
                            // Flat slot (function formal, unsized signal):
                            // runtime bounds from the bound value's width.
                            if let Some((lo, hi)) =
                                self.object_id(&bn).and_then(|id| self.flat_slot_range(id))
                            {
                                let body = self.lower_seq_block(s);
                                return self.make_for(var_id, lo, hi, false, body);
                            }
                            let dims = self
                                .object_id(&bn)
                                .and_then(|id| self.sig_dims.get(&id).cloned())
                                .or_else(|| {
                                    self.types.get(&bn.to_ascii_lowercase()).and_then(|t| {
                                        if let TypeInfo::Array { dims, .. } = t {
                                            Some(dims.clone())
                                        } else {
                                            None
                                        }
                                    })
                                });
                            if let Some(dims) = dims {
                                if let Some(&(lo, n)) = dims.first() {
                                    let hi = lo + n as i64 - 1;
                                    let (a, b, dt) = if attr == "reverse_range" {
                                        (hi, lo, true)
                                    } else {
                                        (lo, hi, false)
                                    };
                                    let body = self.lower_seq_block(s);
                                    return self.make_for(
                                        var_id,
                                        Expr::Lit(Value::from_u64(32, a as u64)),
                                        Expr::Lit(Value::from_u64(32, b as u64)),
                                        dt,
                                        body,
                                    );
                                }
                            }
                            // Memory-backed object without dims (materialized
                            // array constant of an unconstrained type): the
                            // element count is the memory depth; hidden
                            // consts pack element 0 at the top, so the range
                            // is 0 to depth-1.
                            if let Some(depth) = self
                                .object_id(&bn)
                                .and_then(|id| self.mems.get(&id).copied())
                                .filter(|&d| d > 0)
                            {
                                let body = self.lower_seq_block(s);
                                return self.make_for(
                                    var_id,
                                    Expr::Lit(Value::from_u64(32, 0)),
                                    Expr::Lit(Value::from_u64(32, depth as u64 - 1)),
                                    false,
                                    body,
                                );
                            }
                        }
                    }
                }
                let exprs: Vec<&SyntaxNode> =
                    rk.iter().filter(|k| is_expr_node(k.kind())).collect();
                let dir = rk
                    .iter()
                    .find(|k| k.kind() == SyntaxKind::Ident)
                    .map(|k| self.kw(k))
                    .unwrap_or_else(|| "to".to_string());
                let a = exprs
                    .first()
                    .map(|k| self.lower_expr(k, &self.consts.clone()));
                let b = exprs
                    .get(1)
                    .map(|k| self.lower_expr(k, &self.consts.clone()));
                match (a, b) {
                    (Some(a), Some(b)) => {
                        let downto = dir == "downto";
                        if downto {
                            (b, a, true)
                        } else {
                            (a, b, false)
                        }
                    }
                    _ => (
                        Expr::Lit(Value::from_u64(32, 0)),
                        Expr::Lit(Value::from_u64(32, 0)),
                        false,
                    ),
                }
            }
            Some(SyntaxKind::Attr) => {
                // `name'range` on an array signal: 0 to depth-1.
                let ak = real_children(range.expect("matched on range's kind, so range is Some"));
                let base = ak.iter().find(|k| k.kind() == SyntaxKind::Name);
                if let Some(b) = base {
                    let bn = self.text(b);
                    if let Some(id) = self.object_id(&bn) {
                        if let Some(depth) = self.mems.get(&id).copied() {
                            let body = self.lower_seq_block(s);
                            return self.make_for(
                                var_id,
                                Expr::Lit(Value::from_u64(32, 0)),
                                Expr::Lit(Value::from_u64(32, depth as u64 - 1)),
                                false,
                                body,
                            );
                        }
                        // Flat slot (function formal, unsized signal): the
                        // element count comes from the bound value at
                        // runtime / const-eval (`SigAttr`).
                        if let Some((lo, hi)) = self.flat_slot_range(id) {
                            let body = self.lower_seq_block(s);
                            return self.make_for(var_id, lo, hi, false, body);
                        }
                    }
                }
                (
                    Expr::Lit(Value::from_u64(32, 0)),
                    Expr::Lit(Value::from_u64(32, 0)),
                    false,
                )
            }
            _ => (
                Expr::Lit(Value::from_u64(32, 0)),
                Expr::Lit(Value::from_u64(32, 0)),
                false,
            ),
        };
        let body = self.lower_seq_block(s);
        self.make_for(var_id, lo, hi, downto, body)
    }

    pub(super) fn make_for(
        &mut self,
        var_id: SigId,
        lo: Expr,
        hi: Expr,
        downto: bool,
        body: Stmt,
    ) -> Stmt {
        let v = || Expr::Sig(var_id);
        let one = Expr::Lit(Value::from_u64(32, 1));
        let init = Stmt::Assign {
            lhs: v(),
            rhs: lo,
            nba: false,
        };
        let cond = Expr::Bin {
            op: if downto { BinOp::Ge } else { BinOp::Le },
            l: Box::new(v()),
            r: Box::new(hi),
        };
        let step = Stmt::Assign {
            lhs: v(),
            rhs: Expr::Bin {
                op: if downto { BinOp::Sub } else { BinOp::Add },
                l: Box::new(v()),
                r: Box::new(one),
            },
            nba: false,
        };
        Stmt::For {
            init: Box::new(init),
            cond,
            step: Box::new(step),
            body: Box::new(body),
        }
    }

    pub(super) fn lower_loop(&mut self, s: &SyntaxNode) -> Stmt {
        // `while cond loop body end loop` / `loop body end loop`
        let kids = real_children(s);
        let cond = kids
            .iter()
            .find(|k| is_expr_node(k.kind()))
            .map(|k| self.lower_expr(k, &self.consts.clone()))
            .unwrap_or_else(|| Expr::Lit(Value::from_u64(1, 1)));
        let body = self.lower_seq_block(s);
        Stmt::For {
            init: Box::new(Stmt::Null),
            cond,
            step: Box::new(Stmt::Null),
            body: Box::new(body),
        }
    }

    pub(super) fn lower_wait(&mut self, s: &SyntaxNode) -> Stmt {
        let kids = real_children(s);
        // wait [until cond] [on sens] [for time];
        let mut cond = None;
        let mut sens = None;
        let mut time_fs = None;
        let mut time_expr = None;
        let mut i = 0;
        while i < kids.len() {
            let k = &kids[i];
            if k.kind() == SyntaxKind::Ident {
                match self.kw(k).as_str() {
                    "until" => {
                        if let Some(e) = kids.get(i + 1).filter(|e| is_expr_node(e.kind())) {
                            cond = Some(self.lower_expr(e, &self.consts.clone()));
                            i += 1;
                        }
                    }
                    "on" => {
                        sens = kids.get(i + 1).cloned();
                        i += 1;
                    }
                    "for" => {
                        // Physical literal: `10 ns`. The parser may merge number
                        // and unit into one Literal token, or leave the unit as
                        // a separate Ident sibling. A non-literal expression
                        // (`for C_TIMEOUT - now`) lowers to an arbitrary time
                        // expression — evaluated when the wait executes.
                        let num = kids.get(i + 1);
                        if let Some(n) = num.filter(|k| k.kind() == SyntaxKind::Literal) {
                            let t = self.text(n);
                            let (v, mult) = match t.split_once(char::is_whitespace) {
                                Some((a, b)) => (
                                    self.parse_int_literal(a.trim())
                                        .and_then(|v| v.to_u64())
                                        .unwrap_or(0),
                                    time_unit_fs(b.trim()),
                                ),
                                None => {
                                    let unit =
                                        kids.get(i + 2).filter(|k| k.kind() == SyntaxKind::Ident);
                                    (
                                        self.parse_int_literal(&t)
                                            .and_then(|v| v.to_u64())
                                            .unwrap_or(0),
                                        unit.map(|k| time_unit_fs(&self.kw(k)))
                                            .unwrap_or(1_000_000),
                                    )
                                }
                            };
                            time_fs = Some(v.saturating_mul(mult));
                            i += 1;
                        } else if let Some(e) = num.filter(|k| is_expr_node(k.kind())) {
                            time_expr = Some(self.lower_expr(e, &self.consts.clone()));
                            i += 1;
                        }
                    }
                    _ => {}
                }
            }
            i += 1;
        }
        // Timeout for `wait until`/`wait on`: literal fs, or the lowered
        // expression (LRM 8.1: the timeout is evaluated when the wait runs).
        let timeout = time_expr.or_else(|| time_fs.map(|fs| Expr::Lit(Value::from_u64(64, fs))));

        // `wait for T;` — pure timed wait: literal → Delay, expression →
        // timeout-only WaitUntil. No until/on clauses.
        if cond.is_none() && sens.is_none() {
            if let Some(t) = time_fs {
                return Stmt::Delay {
                    time_fs: t,
                    stmt: Box::new(Stmt::Null),
                };
            }
            if let Some(te) = timeout {
                return Stmt::WaitUntil {
                    cond: None,
                    sens: Vec::new(),
                    timeout: Some(te),
                };
            }
            // Bare `wait;` — park forever.
            return Stmt::Wait {
                sens: Vec::new(),
                body: Box::new(Stmt::Null),
            };
        }

        // Explicit `on` list wins over cond-derived edges (LRM 8.1).
        let edges = if let Some(sl) = sens.as_ref() {
            if sl.kind() == SyntaxKind::SensList {
                Some(match self.lower_sens_list(sl) {
                    Sens::List(es) => es,
                    Sens::All => Vec::new(),
                })
            } else {
                None
            }
        } else {
            None
        };
        let edges = edges.or_else(|| cond.as_ref().and_then(|c| edges_from_cond(c, self)));

        // A pure `rising_edge(s)`/`falling_edge(s)` condition is fully encoded
        // by its edge kind — no re-check. Any other condition wakes on any
        // event of its signals and re-checks the full expression (slightly
        // over-sensitive vs the LRM, but never misses the true wake).
        let recheck = match &cond {
            Some(Expr::Rising(inner)) | Some(Expr::Falling(inner)) => {
                !matches!(inner.as_ref(), Expr::Sig(_))
            }
            _ => true,
        };
        if edges.is_none() && timeout.is_none() {
            // No wake source at all: the process can never resume.
            self.warn(
                s.span(),
                "wait statement has no sensitivity, condition signals, or timeout — it can never resume",
            );
        }
        Stmt::WaitUntil {
            cond: if recheck { cond } else { None },
            sens: edges.unwrap_or_default(),
            timeout,
        }
    }
}
