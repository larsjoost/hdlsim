//! Statement/root scans shared by the BMC classify pass: which signals a
//! statement graph writes (`lhs_roots`/`stmt_writes`) and whether it contains
//! an SVA assertion (`stmt_has_sva`). Split out of `bmc.rs` to keep it at its
//! standards baseline.

use hdl_ir::elab::FlatSigId;
use hdl_ir::{Expr, Stmt};

/// Root signals of an LHS chain (`mem[i]`, `q[3:0]`, `{a, b}` recurse).
pub(super) fn lhs_roots(e: &Expr, out: &mut Vec<FlatSigId>) {
    match e {
        Expr::Sig(s) => out.push(*s),
        Expr::Idx { e: x, .. } | Expr::Sel { e: x, .. } => lhs_roots(x, out),
        Expr::Concat(parts) => {
            for p in parts {
                lhs_roots(p, out);
            }
        }
        _ => {}
    }
}

pub(super) fn stmt_writes(s: &Stmt, out: &mut Vec<FlatSigId>) {
    match s {
        Stmt::Spanned { inner, .. } => stmt_writes(inner, out),
        Stmt::Block(ss) => {
            for s in ss {
                stmt_writes(s, out);
            }
        }
        Stmt::If { then, els, .. } => {
            stmt_writes(then, out);
            if let Some(e) = els {
                stmt_writes(e, out);
            }
        }
        Stmt::Case { items, default, .. } => {
            for (_, b) in items {
                stmt_writes(b, out);
            }
            if let Some(d) = default {
                stmt_writes(d, out);
            }
        }
        Stmt::Assign { lhs, .. } | Stmt::AssignDelayed { lhs, .. } => lhs_roots(lhs, out),
        Stmt::Delay { stmt, .. } => stmt_writes(stmt, out),
        Stmt::Wait { body, .. } => stmt_writes(body, out),
        // `wait until` writes nothing; there is no nested body.
        Stmt::WaitUntil { .. } => {}
        Stmt::For {
            init, step, body, ..
        } => {
            stmt_writes(init, out);
            stmt_writes(step, out);
            stmt_writes(body, out);
        }
        Stmt::Repeat { body, .. } => stmt_writes(body, out),
        Stmt::SysCall { .. } | Stmt::Break | Stmt::Continue | Stmt::ExprStmt(_) | Stmt::Null => {}
        Stmt::Return(_) => {}
    }
}

pub(super) fn stmt_has_sva(s: &Stmt) -> bool {
    match s {
        Stmt::Spanned { inner, .. } => stmt_has_sva(inner),
        Stmt::Block(ss) => ss.iter().any(stmt_has_sva),
        Stmt::If { then, els, .. } => {
            stmt_has_sva(then) || els.as_ref().is_some_and(|e| stmt_has_sva(e))
        }
        Stmt::Case { items, default, .. } => {
            items.iter().any(|(_, b)| stmt_has_sva(b))
                || default.as_ref().is_some_and(|d| stmt_has_sva(d))
        }
        Stmt::Assign { .. } | Stmt::AssignDelayed { .. } | Stmt::Return(_) => false,
        Stmt::Delay { stmt, .. } => stmt_has_sva(stmt),
        Stmt::Wait { body, .. } => stmt_has_sva(body),
        Stmt::WaitUntil { .. } => false,
        Stmt::For {
            init, step, body, ..
        } => stmt_has_sva(init) || stmt_has_sva(step) || stmt_has_sva(body),
        Stmt::Repeat { body, .. } => stmt_has_sva(body),
        Stmt::SysCall { name, .. } => name == "sva_assert" || name == "assert",
        Stmt::Break | Stmt::Continue | Stmt::ExprStmt(_) | Stmt::Null => false,
    }
}
