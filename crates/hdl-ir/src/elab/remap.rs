//! Signal-id remapping after flattening: rewrite `Expr`/`Stmt` trees from a
//! module-local signal space into the flat instance-qualified space. Split
//! out of `elab.rs` to keep it at its standards baseline; re-exported
//! unchanged.

/// Remap all signal ids in an expression through `map`.
use super::*;

pub fn remap_expr(e: &Expr, map: &[FlatSigId]) -> Expr {
    match e {
        Expr::Lit(v) => Expr::Lit(v.clone()),
        Expr::Real(x) => Expr::Real(*x),
        Expr::Sig(s) => Expr::Sig(map[*s as usize]),
        Expr::Bin { op, l, r } => Expr::Bin {
            op: *op,
            l: Box::new(remap_expr(l, map)),
            r: Box::new(remap_expr(r, map)),
        },
        Expr::Un { op, e } => Expr::Un {
            op: *op,
            e: Box::new(remap_expr(e, map)),
        },
        Expr::Cond { c, t, e } => Expr::Cond {
            c: Box::new(remap_expr(c, map)),
            t: Box::new(remap_expr(t, map)),
            e: Box::new(remap_expr(e, map)),
        },
        Expr::Call { name, args, shapes } => Expr::Call {
            name: name.clone(),
            args: args.iter().map(|a| remap_expr(a, map)).collect(),
            shapes: shapes
                .iter()
                .map(|b| ShapeBind {
                    formal: b.formal,
                    lo: remap_expr(&b.lo, map),
                    asc: remap_expr(&b.asc, map),
                })
                .collect(),
        },
        Expr::Concat(parts) => Expr::Concat(parts.iter().map(|p| remap_expr(p, map)).collect()),
        Expr::Stream { e, slice } => Expr::Stream {
            e: Box::new(remap_expr(e, map)),
            slice: *slice,
        },
        Expr::Replicate { count, e } => Expr::Replicate {
            count: Box::new(remap_expr(count, map)),
            e: Box::new(remap_expr(e, map)),
        },
        Expr::Rising(e) => Expr::Rising(Box::new(remap_expr(e, map))),
        Expr::Prev(e) => Expr::Prev(Box::new(remap_expr(e, map))),
        Expr::Falling(e) => Expr::Falling(Box::new(remap_expr(e, map))),
        Expr::ToInt(e) => Expr::ToInt(Box::new(remap_expr(e, map))),
        Expr::ToString { e, boolean } => Expr::ToString {
            e: Box::new(remap_expr(e, map)),
            boolean: *boolean,
        },
        Expr::Resize { e, width, signed } => Expr::Resize {
            e: Box::new(remap_expr(e, map)),
            width: *width,
            signed: *signed,
        },
        Expr::Signed(e) => Expr::Signed(Box::new(remap_expr(e, map))),
        Expr::SigAttr { sig, attr } => Expr::SigAttr {
            sig: map[*sig as usize],
            attr,
        },
        Expr::Sel { e, msb, lsb } => Expr::Sel {
            e: Box::new(remap_expr(e, map)),
            msb: Box::new(remap_expr(msb, map)),
            lsb: Box::new(remap_expr(lsb, map)),
        },
        Expr::Idx { e, i } => Expr::Idx {
            e: Box::new(remap_expr(e, map)),
            i: Box::new(remap_expr(i, map)),
        },
    }
}

pub fn remap_stmt(s: &Stmt, map: &[FlatSigId]) -> Stmt {
    match s {
        Stmt::Spanned { span, inner } => Stmt::Spanned {
            span: *span,
            inner: Box::new(remap_stmt(inner, map)),
        },
        Stmt::Block(ss) => Stmt::Block(ss.iter().map(|s| remap_stmt(s, map)).collect()),
        Stmt::Return(e) => Stmt::Return(Box::new(remap_expr(e, map))),
        Stmt::If { cond, then, els } => Stmt::If {
            cond: remap_expr(cond, map),
            then: Box::new(remap_stmt(then, map)),
            els: els.as_ref().map(|e| Box::new(remap_stmt(e, map))),
        },
        Stmt::Case {
            expr,
            items,
            default,
            matching,
        } => Stmt::Case {
            expr: remap_expr(expr, map),
            items: items
                .iter()
                .map(|(cs, b)| {
                    (
                        cs.iter().map(|c| remap_expr(c, map)).collect(),
                        remap_stmt(b, map),
                    )
                })
                .collect(),
            default: default.as_ref().map(|d| Box::new(remap_stmt(d, map))),
            matching: *matching,
        },
        Stmt::Assign { lhs, rhs, nba } => Stmt::Assign {
            lhs: remap_expr(lhs, map),
            rhs: remap_expr(rhs, map),
            nba: *nba,
        },
        Stmt::ProcContAssign { lhs, rhs } => Stmt::ProcContAssign {
            lhs: remap_expr(lhs, map),
            rhs: remap_expr(rhs, map),
        },
        Stmt::Deassign { lhs } => Stmt::Deassign {
            lhs: remap_expr(lhs, map),
        },
        Stmt::AssignDelayed { lhs, rhs, delay } => Stmt::AssignDelayed {
            lhs: remap_expr(lhs, map),
            rhs: remap_expr(rhs, map),
            delay: remap_expr(delay, map),
        },
        Stmt::Delay { time_fs, stmt } => Stmt::Delay {
            time_fs: *time_fs,
            stmt: Box::new(remap_stmt(stmt, map)),
        },
        Stmt::Wait { sens, body } => Stmt::Wait {
            sens: sens
                .iter()
                .map(|e| Edge {
                    sig: map[e.sig as usize],
                    kind: e.kind,
                    iff: e.iff.as_ref().map(|c| remap_expr(c, map)),
                })
                .collect(),
            body: Box::new(remap_stmt(body, map)),
        },
        Stmt::WaitUntil {
            cond,
            sens,
            timeout,
        } => Stmt::WaitUntil {
            cond: cond.as_ref().map(|c| remap_expr(c, map)),
            sens: sens
                .iter()
                .map(|e| Edge {
                    sig: map[e.sig as usize],
                    kind: e.kind,
                    iff: e.iff.as_ref().map(|c| remap_expr(c, map)),
                })
                .collect(),
            timeout: timeout.as_ref().map(|t| remap_expr(t, map)),
        },
        Stmt::SysCall { name, args, shapes } => Stmt::SysCall {
            name: name.clone(),
            args: args.iter().map(|a| remap_expr(a, map)).collect(),
            shapes: shapes
                .iter()
                .map(|b| ShapeBind {
                    formal: b.formal,
                    lo: remap_expr(&b.lo, map),
                    asc: remap_expr(&b.asc, map),
                })
                .collect(),
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
        } => Stmt::For {
            init: Box::new(remap_stmt(init, map)),
            cond: remap_expr(cond, map),
            step: Box::new(remap_stmt(step, map)),
            body: Box::new(remap_stmt(body, map)),
        },
        Stmt::Repeat { count, body } => Stmt::Repeat {
            count: remap_expr(count, map),
            body: Box::new(remap_stmt(body, map)),
        },
        Stmt::Break => Stmt::Break,
        Stmt::Continue => Stmt::Continue,
        Stmt::ExprStmt(e) => Stmt::ExprStmt(remap_expr(e, map)),
        Stmt::Null => Stmt::Null,
    }
}
