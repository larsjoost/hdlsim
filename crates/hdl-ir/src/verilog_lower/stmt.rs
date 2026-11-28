//! Statement lowering: the spanned-statement wrapper, the per-kind
//! dispatcher, and the non-loop, non-assignment statement forms (SVA
//! asserts, blocks, if, case, block-level declarations, return, delay,
//! wait, syscalls). Loop desugaring lives in `stmt_loop.rs`, assignments
//! in `stmt_assign.rs`; split out of `verilog_lower.rs` to keep it at its
//! standards baseline.

use std::collections::HashMap;

use super::const_eval::const_eval;
use super::{is_expr_node, is_stmt_node, real_children, type_kw_width, Lower};
use crate::ir::{Expr, Sens, SigId, SigKind, SignalIr, Stmt};
use crate::value::{UnOp, Value};
use hdl_syntax::cst::SyntaxNode;
use hdl_syntax::diagnostic::Diagnostic;
use hdl_syntax::kinds::SyntaxKind;

impl<'a> Lower<'a> {
    /// Lower one statement, wrapped in a `Stmt::Spanned` with its source span
    /// (drives line/branch coverage). Recursive calls go through this wrapper,
    /// so every nested statement is spanned too.
    /// For-loop init statement. `for (int i = 0; ...)` arrives as a
    /// StmtAssign whose leading type keyword marks it as a declaration:
    /// register the loop variable, then emit its initializer assignment.
    pub(super) fn lower_for_init(&mut self, st: &SyntaxNode, env: &HashMap<String, Value>) -> Stmt {
        if st.kind() == SyntaxKind::StmtAssign {
            let ik = real_children(st);
            let name_pos = ik.iter().position(|k| k.kind() == SyntaxKind::Name);
            let is_decl = name_pos.is_some_and(|np| {
                ik[..np].iter().any(|k| {
                    k.kind() == SyntaxKind::Ident && type_kw_width(&self.text(k)).is_some()
                })
            });
            if is_decl {
                let np = name_pos.expect("is_decl is only true when name_pos was found");
                let n = self.text(&ik[np]);
                if self.sig_id(&n).is_none() {
                    let id = (self.extra_base + self.extra_signals.len()) as SigId;
                    self.extra_signals.push(SignalIr {
                        name: n.clone(),
                        width: 32,
                        kind: SigKind::Wire,
                        init: None,
                        mem: None,
                        signed: false,
                    });
                    self.names.insert(n, id);
                }
                if ik.get(np + 1).is_some_and(|k| self.text(k) == "=") {
                    if let Some(e) = ik.get(np + 2).filter(|e| is_expr_node(e.kind())) {
                        return Stmt::Assign {
                            lhs: Expr::Sig(self.sig_id(&self.text(&ik[np])).expect(
                                "the signal was registered just above when it was missing",
                            )),
                            rhs: self.lower_expr(e, env),
                            nba: false,
                        };
                    }
                }
                return Stmt::Null;
            }
        }
        self.lower_stmt(st, env)
    }

    pub(super) fn lower_stmt(&mut self, s: &SyntaxNode, env: &HashMap<String, Value>) -> Stmt {
        let span = s.span();
        let inner = self.lower_stmt_impl(s, env);
        Stmt::Spanned {
            span,
            inner: Box::new(inner),
        }
    }

    fn lower_stmt_impl(&mut self, s: &SyntaxNode, env: &HashMap<String, Value>) -> Stmt {
        match s.kind() {
            SyntaxKind::SvaStmt => self.lower_sva_stmt(s, env),
            SyntaxKind::Block => Stmt::Block(
                real_children(s)
                    .into_iter()
                    .filter(|k| !matches!(k.kind(), SyntaxKind::Ident | SyntaxKind::Punct))
                    .map(|k| self.lower_stmt(&k, env))
                    .collect(),
            ),
            SyntaxKind::If => self.lower_if_stmt(s, env),
            SyntaxKind::Case => self.lower_case(s, env),
            SyntaxKind::StmtAssign => self.lower_stmt_assign(s, env),
            SyntaxKind::Decl => self.lower_decl_stmt(s, env),
            SyntaxKind::Foreach => self.lower_foreach(s, env),
            // `return expr;` in a function: set the return slot and stop.
            SyntaxKind::ReturnStmt if self.in_function => self.lower_return_stmt(s, env),
            SyntaxKind::Delay => self.lower_delay(s, env),
            SyntaxKind::Wait => self.lower_wait(s, env),
            SyntaxKind::For => self.lower_for_stmt(s, env),
            SyntaxKind::While => self.lower_while(s, env),
            SyntaxKind::Forever => self.lower_forever(s, env),
            SyntaxKind::DoWhile => self.lower_do_while(s, env),
            SyntaxKind::Repeat => self.lower_repeat(s, env),
            SyntaxKind::BreakStmt => Stmt::Break,
            SyntaxKind::ContinueStmt => Stmt::Continue,
            SyntaxKind::Assign => self.lower_proc_cont_assign(s, env),
            SyntaxKind::DeassignStmt => self.lower_deassign(s, env),
            SyntaxKind::SysCall => self.lower_syscall_stmt(s, env),
            _ => Stmt::Null,
        }
    }

    /// Statement-level SVA: `assert (e) [else s];` lowers to the
    /// always-failing `sva_assert` check; assume/cover/expect are
    /// parsed but not simulated.
    fn lower_sva_stmt(&mut self, s: &SyntaxNode, env: &HashMap<String, Value>) -> Stmt {
        let kids = real_children(s);
        let kw = kids
            .iter()
            .find(|k| k.kind() == SyntaxKind::Ident)
            .map(|k| self.text(k))
            .unwrap_or_default();
        if kw != "assert" {
            self.diags.push(Diagnostic::warning(
                "W0401",
                Some(s.span()),
                format!("`{}` statement is parsed but not simulated", kw),
            ));
            return Stmt::Null;
        }
        let expr = kids.iter().find(|k| is_expr_node(k.kind()));
        let els = kids.iter().find(|k| {
            !is_expr_node(k.kind()) && !matches!(k.kind(), SyntaxKind::Ident | SyntaxKind::Punct)
        });
        match expr {
            Some(e) => {
                let e = self.lower_expr(e, env);
                match els {
                    // `assert (e) else s;` == `if (!e) s;`
                    Some(st) => Stmt::If {
                        cond: Expr::Un {
                            op: UnOp::Not,
                            e: Box::new(e),
                        },
                        then: Box::new(self.lower_stmt(st, env)),
                        els: None,
                    },
                    None => Stmt::SysCall {
                        name: "sva_assert".to_string(),
                        args: vec![e],
                        shapes: Vec::new(),
                    },
                }
            }
            None => {
                self.err(s.span(), "malformed assert statement");
                Stmt::Null
            }
        }
    }

    fn lower_if_stmt(&mut self, s: &SyntaxNode, env: &HashMap<String, Value>) -> Stmt {
        let kids = real_children(s);
        let cond_node = kids.iter().find(|k| is_expr_node(k.kind()));
        let stmt_nodes: Vec<&SyntaxNode> = kids.iter().filter(|k| is_stmt_node(k.kind())).collect();
        let cond = cond_node
            .map(|c| self.lower_expr(c, env))
            .unwrap_or_else(|| Expr::Lit(Value::from_u64(1, 0)));
        let then = stmt_nodes
            .first()
            .map(|b| Box::new(self.lower_stmt(b, env)))
            .unwrap_or_else(|| Box::new(Stmt::Null));
        let els = stmt_nodes.get(1).map(|b| Box::new(self.lower_stmt(b, env)));
        Stmt::If { cond, then, els }
    }

    /// Block-level declaration (`int x = 3;` inside begin/end):
    /// allocate a hidden signal, then lower any init as an assignment.
    fn lower_decl_stmt(&mut self, s: &SyntaxNode, env: &HashMap<String, Value>) -> Stmt {
        let dk = real_children(s);
        let width = self.decl_width(&dk, env);
        let tname = self.decl_type_name(&dk);
        let mut inits = Vec::new();
        for (i, k) in dk.iter().enumerate() {
            if k.kind() == SyntaxKind::Name {
                self.decl_signal_and_init(&dk, i, k, width, tname.as_deref(), env, &mut inits);
            }
        }
        if inits.is_empty() {
            Stmt::Null
        } else if inits.len() == 1 {
            inits.pop().expect("checked inits.len() == 1")
        } else {
            Stmt::Block(inits)
        }
    }

    /// Register one declared name (hidden signal, mem/type bookkeeping) and
    /// collect its `= expr` initializer, if any.
    #[allow(clippy::too_many_arguments)]
    fn decl_signal_and_init(
        &mut self,
        dk: &[SyntaxNode],
        i: usize,
        k: &SyntaxNode,
        width: usize,
        tname: Option<&str>,
        env: &HashMap<String, Value>,
        inits: &mut Vec<Stmt>,
    ) {
        let n = self.text(k);
        if self.sig_id(&n).is_none() {
            let id = (self.extra_base + self.extra_signals.len()) as SigId;
            let mem = dk.get(i + 1).and_then(|r| {
                if r.kind() == SyntaxKind::Range {
                    Some(self.range_depth(r, env))
                } else {
                    None
                }
            });
            self.extra_signals.push(SignalIr {
                name: n.clone(),
                width,
                kind: SigKind::Wire,
                init: None,
                mem,
                signed: false,
            });
            if mem.is_some() {
                self.sig_is_mem.insert(id);
            }
            if let Some(t) = tname {
                self.sig_types.insert(id, t.to_string());
            }
            self.names.insert(n, id);
        }
        // `= expr` init?
        if dk.get(i + 1).is_some_and(|k| self.text(k) == "=") {
            if let Some(e) = dk.get(i + 2).filter(|e| is_expr_node(e.kind())) {
                inits.push(Stmt::Assign {
                    lhs: Expr::Sig(
                        self.sig_id(&self.text(k))
                            .expect("the signal was registered earlier in this declaration"),
                    ),
                    rhs: self.lower_expr(e, env),
                    nba: false,
                });
            }
        }
    }

    fn lower_return_stmt(&mut self, s: &SyntaxNode, env: &HashMap<String, Value>) -> Stmt {
        let kids = real_children(s);
        let e = kids.iter().find(|k| is_expr_node(k.kind()));
        Stmt::Return(Box::new(
            e.map(|e| self.lower_expr(e, env))
                .unwrap_or_else(|| Expr::Lit(Value::from_u64(1, 0))),
        ))
    }

    fn lower_delay(&mut self, s: &SyntaxNode, env: &HashMap<String, Value>) -> Stmt {
        let kids = real_children(s);
        let value = kids.iter().find(|k| is_expr_node(k.kind()));
        let stmt = kids.iter().find(|k| is_stmt_node(k.kind()));
        let time_fs = value
            .map(|v| self.lower_expr(v, env))
            .and_then(|e| const_eval(&e, env))
            .and_then(|v| v.to_u64())
            .map(|t| t * self.time_unit_fs)
            .unwrap_or(0);
        let stmt = stmt
            .map(|b| Box::new(self.lower_stmt(b, env)))
            .unwrap_or_else(|| Box::new(Stmt::Null));
        Stmt::Delay { time_fs, stmt }
    }

    fn lower_wait(&mut self, s: &SyntaxNode, env: &HashMap<String, Value>) -> Stmt {
        let kids = real_children(s);
        let sens_node = kids.iter().find(|k| k.kind() == SyntaxKind::SensList);
        let body = kids
            .iter()
            .find(|k| is_stmt_node(k.kind()) && k.kind() != SyntaxKind::SensList);
        let edges = match sens_node.map(|sl| self.lower_sens_list(sl, env)) {
            Some(Sens::List(es)) => es,
            _ => Vec::new(),
        };
        let body = body
            .map(|b| Box::new(self.lower_stmt(b, env)))
            .unwrap_or_else(|| Box::new(Stmt::Null));
        Stmt::Wait { sens: edges, body }
    }

    fn lower_case(&mut self, s: &SyntaxNode, env: &HashMap<String, Value>) -> Stmt {
        let kids = real_children(s);
        let expr = kids
            .iter()
            .find(|k| is_expr_node(k.kind()))
            .map(|k| self.lower_expr(k, env))
            .unwrap_or_else(|| Expr::Lit(Value::from_u64(1, 0)));
        let mut items = Vec::new();
        let mut default = None;
        for item in kids.iter().filter(|k| k.kind() == SyntaxKind::CaseItem) {
            let ik = real_children(item);
            let is_default = ik
                .iter()
                .any(|k| k.kind() == SyntaxKind::Ident && self.text(k) == "default");
            let conds = ik
                .iter()
                .filter(|k| is_expr_node(k.kind()))
                .map(|k| self.lower_expr(k, env))
                .collect::<Vec<_>>();
            let body = ik
                .iter()
                .find(|k| is_stmt_node(k.kind()))
                .map(|k| self.lower_stmt(k, env))
                .unwrap_or(Stmt::Null);
            if is_default {
                default = Some(Box::new(body));
            } else {
                items.push((conds, body));
            }
        }
        Stmt::Case {
            expr,
            items,
            default,
            matching: false,
        }
    }

    fn lower_syscall_stmt(&mut self, s: &SyntaxNode, env: &HashMap<String, Value>) -> Stmt {
        let kids = real_children(s);
        let name = kids
            .first()
            .map(|k| self.text(k).trim_start_matches('$').to_string())
            .unwrap_or_default();
        let args = kids[1..]
            .iter()
            .filter(|k| is_expr_node(k.kind()))
            .map(|k| self.lower_expr(k, env))
            .collect();
        Stmt::SysCall {
            name,
            args,
            shapes: Vec::new(),
        }
    }
}
