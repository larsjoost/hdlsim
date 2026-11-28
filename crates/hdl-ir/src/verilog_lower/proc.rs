//! Process lowering: `always`/`initial`/`final` blocks, their sensitivity
//! lists (posedge/negedge/`*`, SV `iff` qualifiers), and the ProcessIr they
//! produce. Split out of `verilog_lower.rs` to keep it at its standards
//! baseline.

use std::collections::HashMap;

use crate::ir::{Edge, EdgeKind, Expr, ModuleIr, ProcKind, ProcessIr, Sens, Stmt};
use crate::value::Value;
use hdl_syntax::cst::SyntaxNode;
use hdl_syntax::kinds::SyntaxKind;

use super::{is_expr_node, real_children, Lower};

impl<'a> Lower<'a> {
    pub(super) fn lower_process(
        &mut self,
        module: &mut ModuleIr,
        p: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) {
        let kind = if p.kind() == SyntaxKind::Initial {
            ProcKind::Initial
        } else {
            ProcKind::Always
        };
        let kids = real_children(p);
        // No sensitivity list at all (`always #5 ...`): an empty list means
        // "no event sensitivity — start once at t=0 and loop on the body".
        // `@*` lowers to Sens::All via the SensList arm below.
        let mut sens = Sens::List(Vec::new());
        let mut body = None;
        let mut origin = String::new();
        for k in &kids {
            match k.kind() {
                SyntaxKind::SensList => {
                    sens = self.lower_sens_list(k, env);
                    let kw = match kind {
                        ProcKind::Initial => "initial",
                        ProcKind::Always => "always",
                    };
                    origin = format!("{} @{}", kw, self.text(k));
                }
                SyntaxKind::Ident | SyntaxKind::Punct => {}
                _ => body = Some(k.clone()),
            }
        }
        let body = match body {
            Some(b) => self.lower_stmt(&b, env),
            None => Stmt::Null,
        };
        if origin.is_empty() {
            origin = match kind {
                ProcKind::Initial => "initial".to_string(),
                ProcKind::Always => "always".to_string(),
            };
        }
        module.processes.push(ProcessIr {
            kind,
            sens,
            body,
            origin,
            gen_scope: self.gen_path.join("."),
            initial: false,
        });
    }

    pub(super) fn lower_sens_list(&mut self, s: &SyntaxNode, env: &HashMap<String, Value>) -> Sens {
        let mut edges: Vec<Edge> = Vec::new();
        let mut pending = EdgeKind::Any;
        for c in real_children(s) {
            match c.kind() {
                SyntaxKind::Ident => {
                    let t = self.text(&c);
                    if t == "posedge" {
                        pending = EdgeKind::Pos;
                    } else if t == "negedge" {
                        pending = EdgeKind::Neg;
                    }
                    // `or` separators carry no meaning beyond joining the list.
                }
                SyntaxKind::IffCond => {
                    // Event qualifier on the entry lowered just before it.
                    let cond = real_children(&c)
                        .into_iter()
                        .find(|k| is_expr_node(k.kind()))
                        .map(|k| self.lower_expr(&k, env));
                    match edges.last_mut() {
                        Some(e) => e.iff = cond,
                        None => self.err(c.span(), "`iff` qualifier without a preceding event"),
                    }
                }
                _ if is_expr_node(c.kind()) => {
                    let e = self.lower_expr(&c, env);
                    // `a`, `a[1:0]`, `a[i]` — the entry's base signal.
                    let base_sig = match &e {
                        Expr::Sig(id) => Some(*id),
                        Expr::Idx { e, .. } | Expr::Sel { e, .. } => match &**e {
                            Expr::Sig(id) => Some(*id),
                            _ => None,
                        },
                        _ => None,
                    };
                    match base_sig {
                        Some(id) => edges.push(Edge {
                            sig: id,
                            kind: pending,
                            iff: None,
                        }),
                        None => self.err(c.span(), "sensitivity list entry must be a signal"),
                    }
                    pending = EdgeKind::Any;
                }
                _ => {}
            }
        }
        if edges.is_empty() {
            Sens::All
        } else {
            Sens::List(edges)
        }
    }
}
