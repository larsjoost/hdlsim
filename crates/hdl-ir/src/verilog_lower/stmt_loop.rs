//! Loop statement lowering: `foreach` (desugared to an indexed `for` over
//! `$array_size`), `for`, `while`, `forever`, `do..while`, and `repeat`.
//! Split out of `verilog_lower.rs` (statements) to keep the modules at the
//! module-size standard.

use std::collections::HashMap;

use super::{is_expr_node, is_stmt_node, real_children, Lower};
use crate::ir::{Expr, SigId, SigKind, SignalIr, Stmt};
use crate::value::{BinOp, Value};
use hdl_syntax::cst::SyntaxNode;
use hdl_syntax::kinds::SyntaxKind;

impl<'a> Lower<'a> {
    /// `foreach (arr[i]) body` → for (i = 0; i < arr.size(); i++) body
    pub(super) fn lower_foreach(&mut self, s: &SyntaxNode, env: &HashMap<String, Value>) -> Stmt {
        let kids = real_children(s);
        let hdr = kids.iter().find(|k| is_expr_node(k.kind())).cloned();
        let body_node = kids.iter().find(|k| is_stmt_node(k.kind())).cloned();
        let (arr_node, idx_name) = self.foreach_header(hdr.as_ref());
        let Some(arr_node) = arr_node else {
            self.err(s.span(), "foreach needs an array");
            return Stmt::Null;
        };
        let id = (self.extra_base + self.extra_signals.len()) as SigId;
        self.extra_signals.push(SignalIr {
            name: format!("__foreach_{}", idx_name),
            width: 32,
            kind: SigKind::Wire,
            init: None,
            mem: None,
            signed: false,
        });
        self.names.insert(idx_name, id);
        let arr = self.lower_expr(&arr_node, env);
        let idx = Expr::Sig(id);
        let body = body_node
            .map(|b| Box::new(self.lower_stmt(&b, env)))
            .unwrap_or_else(|| Box::new(Stmt::Null));
        foreach_loop(idx, arr, body)
    }

    /// Split the foreach header into the array node and the index name
    /// (`arr[i]`, default index `i`).
    fn foreach_header(&self, hdr: Option<&SyntaxNode>) -> (Option<SyntaxNode>, String) {
        match hdr {
            Some(h) if h.kind() == SyntaxKind::Index => {
                let ik = real_children(h);
                let exprs: Vec<&SyntaxNode> =
                    ik.iter().filter(|k| is_expr_node(k.kind())).collect();
                let arr = exprs.first().map(|n| (*n).clone());
                let idx = exprs
                    .get(1)
                    .filter(|n| n.kind() == SyntaxKind::Name)
                    .map(|n| self.text(n))
                    .unwrap_or_else(|| "i".to_string());
                (arr, idx)
            }
            Some(h) => (Some(h.clone()), "i".to_string()),
            None => (None, "i".to_string()),
        }
    }

    pub(super) fn lower_for_stmt(&mut self, s: &SyntaxNode, env: &HashMap<String, Value>) -> Stmt {
        let kids = real_children(s);
        let stmts: Vec<&SyntaxNode> = kids.iter().filter(|k| is_stmt_node(k.kind())).collect();
        // Lower the init first: `for (int i = 0; ...)` declares `i`,
        // which the condition, step and body reference.
        let init = stmts
            .first()
            .map(|st| self.lower_for_init(st, env))
            .unwrap_or(Stmt::Null);
        let cond = kids
            .iter()
            .find(|k| is_expr_node(k.kind()))
            .map(|c| self.lower_expr(c, env))
            .unwrap_or_else(|| Expr::Lit(Value::from_u64(1, 1)));
        if stmts.len() >= 3 {
            Stmt::For {
                init: Box::new(init),
                cond,
                step: Box::new(self.lower_stmt(stmts[1], env)),
                body: Box::new(self.lower_stmt(stmts[2], env)),
            }
        } else {
            Stmt::Null
        }
    }

    /// Desugar `while (c) body` into a For loop with no init/step.
    pub(super) fn lower_while(&mut self, s: &SyntaxNode, env: &HashMap<String, Value>) -> Stmt {
        let kids = real_children(s);
        let cond = kids
            .iter()
            .find(|k| is_expr_node(k.kind()))
            .map(|c| self.lower_expr(c, env))
            .unwrap_or_else(|| Expr::Lit(Value::from_u64(1, 1)));
        let body = kids
            .iter()
            .find(|k| is_stmt_node(k.kind()))
            .map(|b| Box::new(self.lower_stmt(b, env)))
            .unwrap_or_else(|| Box::new(Stmt::Null));
        Stmt::For {
            init: Box::new(Stmt::Null),
            cond,
            step: Box::new(Stmt::Null),
            body,
        }
    }

    pub(super) fn lower_forever(&mut self, s: &SyntaxNode, env: &HashMap<String, Value>) -> Stmt {
        let kids = real_children(s);
        let body = kids
            .iter()
            .find(|k| is_stmt_node(k.kind()))
            .map(|b| Box::new(self.lower_stmt(b, env)))
            .unwrap_or_else(|| Box::new(Stmt::Null));
        Stmt::For {
            init: Box::new(Stmt::Null),
            cond: Expr::Lit(Value::from_u64(1, 1)),
            step: Box::new(Stmt::Null),
            body,
        }
    }

    /// `do body while (c)` = body; while (c) body;
    pub(super) fn lower_do_while(&mut self, s: &SyntaxNode, env: &HashMap<String, Value>) -> Stmt {
        let kids = real_children(s);
        let stmts: Vec<&SyntaxNode> = kids.iter().filter(|k| is_stmt_node(k.kind())).collect();
        let cond = kids
            .iter()
            .find(|k| is_expr_node(k.kind()))
            .map(|c| self.lower_expr(c, env))
            .unwrap_or_else(|| Expr::Lit(Value::from_u64(0, 1)));
        match stmts.first() {
            Some(b) => {
                let body = self.lower_stmt(b, env);
                Stmt::Block(vec![
                    body.clone(),
                    Stmt::For {
                        init: Box::new(Stmt::Null),
                        cond,
                        step: Box::new(Stmt::Null),
                        body: Box::new(body),
                    },
                ])
            }
            None => Stmt::Null,
        }
    }

    pub(super) fn lower_repeat(&mut self, s: &SyntaxNode, env: &HashMap<String, Value>) -> Stmt {
        let kids = real_children(s);
        let count = kids
            .iter()
            .find(|k| is_expr_node(k.kind()))
            .map(|c| self.lower_expr(c, env))
            .unwrap_or_else(|| Expr::Lit(Value::from_u64(0, 32)));
        let body = kids
            .iter()
            .find(|k| is_stmt_node(k.kind()))
            .map(|b| Box::new(self.lower_stmt(b, env)))
            .unwrap_or_else(|| Box::new(Stmt::Null));
        Stmt::Repeat { count, body }
    }
}

/// The desugared `for` loop for `foreach`: index from 0 up to
/// `$array_size(arr)`, stepping by one.
fn foreach_loop(idx: Expr, arr: Expr, body: Box<Stmt>) -> Stmt {
    Stmt::For {
        init: Box::new(Stmt::Assign {
            lhs: idx.clone(),
            rhs: Expr::Lit(Value::from_u64(32, 0)),
            nba: false,
        }),
        cond: Expr::Bin {
            op: BinOp::Lt,
            l: Box::new(idx.clone()),
            r: Box::new(Expr::Call {
                name: "$array_size".to_string(),
                args: vec![arr],
                shapes: Vec::new(),
            }),
        },
        step: Box::new(Stmt::Assign {
            lhs: idx.clone(),
            rhs: Expr::Bin {
                op: BinOp::Add,
                l: Box::new(idx),
                r: Box::new(Expr::Lit(Value::from_u64(32, 1))),
            },
            nba: false,
        }),
        body,
    }
}
