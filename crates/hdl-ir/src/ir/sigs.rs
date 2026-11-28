//! Signal-reference collection over the IR: `Expr::collect_sigs` and
//! `Stmt::collect_sigs` — every `SigId` an expression or statement reads,
//! used for `@*` sensitivity and VHDL `(ALL)` lists. Split out of `ir.rs`
//! to keep it at its standards baseline.

use super::{Expr, SigId, Stmt};

impl Expr {
    /// All signal ids referenced by this expression (for `@*` sensitivity).
    pub fn collect_sigs(&self, out: &mut Vec<SigId>) {
        match self {
            Expr::Lit(_) | Expr::Real(_) => {}
            Expr::SigAttr { sig, .. } => out.push(*sig),
            Expr::Sig(s) => out.push(*s),
            Expr::Bin { l, r, .. } => {
                l.collect_sigs(out);
                r.collect_sigs(out);
            }
            Expr::Un { e, .. } => e.collect_sigs(out),
            Expr::Cond { c, t, e } => {
                c.collect_sigs(out);
                t.collect_sigs(out);
                e.collect_sigs(out);
            }
            Expr::Call { args, shapes, .. } => {
                for a in args {
                    a.collect_sigs(out);
                }
                for b in shapes {
                    b.lo.collect_sigs(out);
                    b.asc.collect_sigs(out);
                }
            }
            Expr::Stream { e, .. } => e.collect_sigs(out),
            Expr::Concat(parts) => {
                for p in parts {
                    p.collect_sigs(out);
                }
            }
            Expr::Replicate { count, e } => {
                count.collect_sigs(out);
                e.collect_sigs(out);
            }
            Expr::Sel { e, msb, lsb } => {
                e.collect_sigs(out);
                msb.collect_sigs(out);
                lsb.collect_sigs(out);
            }
            Expr::Idx { e, i } => {
                e.collect_sigs(out);
                i.collect_sigs(out);
            }
            Expr::Rising(e)
            | Expr::Falling(e)
            | Expr::Prev(e)
            | Expr::ToInt(e)
            | Expr::ToString { e, .. } => e.collect_sigs(out),
            Expr::Resize { e, .. } => e.collect_sigs(out),
            Expr::Signed(e) => e.collect_sigs(out),
        }
    }
}

impl Stmt {
    pub fn collect_sigs(&self, out: &mut Vec<SigId>) {
        match self {
            Stmt::Spanned { inner, .. } => inner.collect_sigs(out),
            Stmt::Return(e) => e.collect_sigs(out),
            Stmt::Block(ss) => {
                for s in ss {
                    s.collect_sigs(out);
                }
            }
            Stmt::If { cond, then, els } => {
                cond.collect_sigs(out);
                then.collect_sigs(out);
                if let Some(e) = els {
                    e.collect_sigs(out);
                }
            }
            Stmt::Case {
                expr,
                items,
                default,
                ..
            } => {
                expr.collect_sigs(out);
                for (conds, body) in items {
                    for c in conds {
                        c.collect_sigs(out);
                    }
                    body.collect_sigs(out);
                }
                if let Some(d) = default {
                    d.collect_sigs(out);
                }
            }
            Stmt::Assign { lhs, rhs, .. } => {
                lhs.collect_sigs(out);
                rhs.collect_sigs(out);
            }
            Stmt::AssignDelayed { lhs, rhs, delay } => {
                lhs.collect_sigs(out);
                rhs.collect_sigs(out);
                delay.collect_sigs(out);
            }
            Stmt::Delay { stmt, .. } => stmt.collect_sigs(out),
            Stmt::Wait { sens, body } => {
                for e in sens {
                    out.push(e.sig);
                }
                body.collect_sigs(out);
            }
            Stmt::WaitUntil {
                cond,
                sens,
                timeout,
            } => {
                for e in sens {
                    out.push(e.sig);
                }
                if let Some(c) = cond {
                    c.collect_sigs(out);
                }
                if let Some(t) = timeout {
                    t.collect_sigs(out);
                }
            }
            Stmt::SysCall { args, .. } => {
                for a in args {
                    a.collect_sigs(out);
                }
            }
            Stmt::For {
                init,
                cond,
                step,
                body,
            } => {
                init.collect_sigs(out);
                cond.collect_sigs(out);
                step.collect_sigs(out);
                body.collect_sigs(out);
            }
            Stmt::Repeat { count, body } => {
                count.collect_sigs(out);
                body.collect_sigs(out);
            }
            Stmt::Break | Stmt::Continue => {}
            Stmt::ExprStmt(e) => e.collect_sigs(out),
            Stmt::Null => {}
        }
    }
}
