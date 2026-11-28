//! Checkpoint codec for the IR nodes: `Expr`, `Stmt`, and the shared
//! call-payload (name + args + shape binds) used by both. Tag bytes match
//! the wire order; the operator enums live in the sibling `op.rs`. Pure
//! move from `ckpt.rs`.

use super::*;

/// Write a call node's payload — name, args, then the shape binds. Shared by
/// `Expr::Call` and `Stmt::SysCall`, which differ only in their tag byte.
pub(super) fn w_call(w: &mut W, name: &str, args: &[Expr], shapes: &[ShapeBind]) {
    w.str(name);
    w.u32(args.len() as u32);
    for a in args {
        w_expr(w, a);
    }
    w.u32(shapes.len() as u32);
    for b in shapes {
        w.u32(b.formal as u32);
        w_expr(w, &b.lo);
        w_expr(w, &b.asc);
    }
}

pub(super) fn w_expr(w: &mut W, e: &Expr) {
    match e {
        Expr::Lit(v) => {
            w.u8(0);
            w.value(v);
        }
        Expr::Sig(id) => {
            w.u8(1);
            w.u32(*id);
        }
        Expr::Bin { op, l, r } => {
            w.u8(2);
            w_binop(w, *op);
            w_expr(w, l);
            w_expr(w, r);
        }
        Expr::Un { op, e } => {
            w.u8(3);
            w_unop(w, *op);
            w_expr(w, e);
        }
        Expr::Cond { c, t, e } => {
            w.u8(4);
            w_expr(w, c);
            w_expr(w, t);
            w_expr(w, e);
        }
        Expr::Call { name, args, shapes } => {
            w.u8(5);
            w_call(w, name, args, shapes);
        }
        Expr::Concat(parts) => {
            w.u8(6);
            w.u32(parts.len() as u32);
            for p in parts {
                w_expr(w, p);
            }
        }
        Expr::Replicate { count, e } => {
            w.u8(7);
            w_expr(w, count);
            w_expr(w, e);
        }
        Expr::Stream { e, slice } => {
            w.u8(19);
            w_expr(w, e);
            w.u32(*slice);
        }
        Expr::Sel { e, msb, lsb } => {
            w.u8(8);
            w_expr(w, e);
            w_expr(w, msb);
            w_expr(w, lsb);
        }
        Expr::Idx { e, i } => {
            w.u8(9);
            w_expr(w, e);
            w_expr(w, i);
        }
        Expr::Rising(e) => {
            w.u8(10);
            w_expr(w, e);
        }
        Expr::Falling(e) => {
            w.u8(11);
            w_expr(w, e);
        }
        Expr::Prev(e) => {
            w.u8(17);
            w_expr(w, e);
        }
        Expr::ToInt(e) => {
            w.u8(12);
            w_expr(w, e);
        }
        Expr::ToString { e, boolean } => {
            w.u8(13);
            w_expr(w, e);
            w.bool(*boolean);
        }
        Expr::Resize { e, width, signed } => {
            w.u8(14);
            w_expr(w, e);
            w.usize(*width);
            w.bool(*signed);
        }
        Expr::Signed(e) => {
            w.u8(15);
            w_expr(w, e);
        }
        Expr::Real(x) => {
            w.u8(16);
            w.u64(x.to_bits());
        }
        Expr::SigAttr { sig, attr } => {
            w.u8(18);
            w.u32(*sig);
            w.str(attr);
        }
    }
}

/// Read a [`w_call`] payload back: name, args, shape binds.
pub(super) fn r_call(r: &mut R) -> Result<(String, Vec<Expr>, Vec<ShapeBind>), String> {
    let name = r.str()?;
    let n = r.u32()? as usize;
    if n > (1 << 20) {
        return Err("too many args".to_string());
    }
    let mut args = Vec::with_capacity(n);
    for _ in 0..n {
        args.push(r_expr(r)?);
    }
    let ns = r.u32()? as usize;
    if ns > (1 << 12) {
        return Err("too many shape binds".to_string());
    }
    let mut shapes = Vec::with_capacity(ns);
    for _ in 0..ns {
        let formal = r.u32()? as usize;
        shapes.push(ShapeBind {
            formal,
            lo: r_expr(r)?,
            asc: r_expr(r)?,
        });
    }
    Ok((name, args, shapes))
}

pub(super) fn r_expr(r: &mut R) -> Result<Expr, String> {
    Ok(match r.u8()? {
        0 => Expr::Lit(r.value()?),
        1 => Expr::Sig(r.u32()?),
        2 => Expr::Bin {
            op: r_binop(r)?,
            l: Box::new(r_expr(r)?),
            r: Box::new(r_expr(r)?),
        },
        3 => Expr::Un {
            op: r_unop(r)?,
            e: Box::new(r_expr(r)?),
        },
        4 => Expr::Cond {
            c: Box::new(r_expr(r)?),
            t: Box::new(r_expr(r)?),
            e: Box::new(r_expr(r)?),
        },
        5 => {
            let (name, args, shapes) = r_call(r)?;
            Expr::Call { name, args, shapes }
        }
        6 => {
            let n = r.u32()? as usize;
            if n > (1 << 20) {
                return Err("too many concat parts".to_string());
            }
            let mut parts = Vec::with_capacity(n);
            for _ in 0..n {
                parts.push(r_expr(r)?);
            }
            Expr::Concat(parts)
        }
        7 => Expr::Replicate {
            count: Box::new(r_expr(r)?),
            e: Box::new(r_expr(r)?),
        },
        8 => Expr::Sel {
            e: Box::new(r_expr(r)?),
            msb: Box::new(r_expr(r)?),
            lsb: Box::new(r_expr(r)?),
        },
        9 => Expr::Idx {
            e: Box::new(r_expr(r)?),
            i: Box::new(r_expr(r)?),
        },
        10 => Expr::Rising(Box::new(r_expr(r)?)),
        11 => Expr::Falling(Box::new(r_expr(r)?)),
        17 => Expr::Prev(Box::new(r_expr(r)?)),
        19 => Expr::Stream {
            e: Box::new(r_expr(r)?),
            slice: r.u32()?,
        },
        12 => Expr::ToInt(Box::new(r_expr(r)?)),
        13 => Expr::ToString {
            e: Box::new(r_expr(r)?),
            boolean: r.bool()?,
        },
        14 => Expr::Resize {
            e: Box::new(r_expr(r)?),
            width: r.usize()?,
            signed: r.bool()?,
        },
        15 => Expr::Signed(Box::new(r_expr(r)?)),
        16 => Expr::Real(f64::from_bits(r.u64()?)),
        18 => {
            let sig = r.u32()?;
            let attr = match r.str()?.as_str() {
                "length" => "length",
                "high" => "high",
                "low" => "low",
                "left" => "left",
                "right" => "right",
                other => return Err(format!("bad SigAttr attr {other}")),
            };
            Expr::SigAttr { sig, attr }
        }
        t => return Err(format!("bad Expr tag {}", t)),
    })
}

pub(super) fn w_stmt(w: &mut W, s: &Stmt) {
    match s {
        Stmt::Block(ss) => {
            w.u8(0);
            w.u32(ss.len() as u32);
            for s in ss {
                w_stmt(w, s);
            }
        }
        Stmt::If { cond, then, els } => {
            w.u8(1);
            w_expr(w, cond);
            w_stmt(w, then);
            w.bool(els.is_some());
            if let Some(e) = els {
                w_stmt(w, e);
            }
        }
        Stmt::Case {
            expr,
            items,
            default,
            matching,
        } => {
            w.u8(2);
            w.bool(*matching);
            w_expr(w, expr);
            w.u32(items.len() as u32);
            for (pats, body) in items {
                w.u32(pats.len() as u32);
                for p in pats {
                    w_expr(w, p);
                }
                w_stmt(w, body);
            }
            w.bool(default.is_some());
            if let Some(d) = default {
                w_stmt(w, d);
            }
        }
        Stmt::Assign { lhs, rhs, nba } => {
            w.u8(3);
            w_expr(w, lhs);
            w_expr(w, rhs);
            w.bool(*nba);
        }
        Stmt::AssignDelayed { lhs, rhs, delay } => {
            w.u8(15);
            w_expr(w, lhs);
            w_expr(w, rhs);
            w_expr(w, delay);
        }
        Stmt::Delay { time_fs, stmt } => {
            w.u8(4);
            w.u64(*time_fs);
            w_stmt(w, stmt);
        }
        Stmt::Wait { sens, body } => {
            w.u8(5);
            w.u32(sens.len() as u32);
            for e in sens {
                w_edge(w, e);
            }
            w_stmt(w, body);
        }
        Stmt::WaitUntil {
            cond,
            sens,
            timeout,
        } => {
            w.u8(16);
            match cond {
                Some(c) => {
                    w.bool(true);
                    w_expr(w, c);
                }
                None => w.bool(false),
            }
            w.u32(sens.len() as u32);
            for e in sens {
                w_edge(w, e);
            }
            match timeout {
                Some(t) => {
                    w.bool(true);
                    w_expr(w, t);
                }
                None => w.bool(false),
            }
        }
        Stmt::SysCall { name, args, shapes } => {
            w.u8(6);
            w_call(w, name, args, shapes);
        }
        Stmt::For {
            init,
            cond,
            step,
            body,
        } => {
            w.u8(7);
            w_stmt(w, init);
            w_expr(w, cond);
            w_stmt(w, step);
            w_stmt(w, body);
        }
        Stmt::Repeat { count, body } => {
            w.u8(8);
            w_expr(w, count);
            w_stmt(w, body);
        }
        Stmt::ExprStmt(e) => {
            w.u8(9);
            w_expr(w, e);
        }
        Stmt::Return(e) => {
            w.u8(14);
            w_expr(w, e);
        }
        Stmt::Break => w.u8(10),
        Stmt::Continue => w.u8(11),
        Stmt::Null => w.u8(12),
        Stmt::ProcContAssign { lhs, rhs } => {
            w.u8(17);
            w_expr(w, lhs);
            w_expr(w, rhs);
        }
        Stmt::Deassign { lhs } => {
            w.u8(18);
            w_expr(w, lhs);
        }
        Stmt::Spanned { span, inner } => {
            w.u8(13);
            w_span(w, span);
            w_stmt(w, inner);
        }
    }
}

pub(super) fn r_stmt(r: &mut R) -> Result<Stmt, String> {
    Ok(match r.u8()? {
        0 => {
            let n = r.u32()? as usize;
            if n > (1 << 20) {
                return Err("too many stmts".to_string());
            }
            let mut ss = Vec::with_capacity(n);
            for _ in 0..n {
                ss.push(r_stmt(r)?);
            }
            Stmt::Block(ss)
        }
        1 => {
            let cond = r_expr(r)?;
            let then = Box::new(r_stmt(r)?);
            let els = if r.bool()? {
                Some(Box::new(r_stmt(r)?))
            } else {
                None
            };
            Stmt::If { cond, then, els }
        }
        2 => {
            let matching = r.bool()?;
            let expr = r_expr(r)?;
            let n = r.u32()? as usize;
            if n > (1 << 20) {
                return Err("too many case items".to_string());
            }
            let mut items = Vec::with_capacity(n);
            for _ in 0..n {
                let np = r.u32()? as usize;
                if np > (1 << 16) {
                    return Err("too many case pats".to_string());
                }
                let mut pats = Vec::with_capacity(np);
                for _ in 0..np {
                    pats.push(r_expr(r)?);
                }
                let body = r_stmt(r)?;
                items.push((pats, body));
            }
            let default = if r.bool()? {
                Some(Box::new(r_stmt(r)?))
            } else {
                None
            };
            Stmt::Case {
                expr,
                items,
                default,
                matching,
            }
        }
        3 => Stmt::Assign {
            lhs: r_expr(r)?,
            rhs: r_expr(r)?,
            nba: r.bool()?,
        },
        15 => Stmt::AssignDelayed {
            lhs: r_expr(r)?,
            rhs: r_expr(r)?,
            delay: r_expr(r)?,
        },
        4 => Stmt::Delay {
            time_fs: r.u64()?,
            stmt: Box::new(r_stmt(r)?),
        },
        5 => {
            let n = r.u32()? as usize;
            if n > (1 << 16) {
                return Err("too many sens edges".to_string());
            }
            let mut sens = Vec::with_capacity(n);
            for _ in 0..n {
                sens.push(r_edge(r)?);
            }
            let body = Box::new(r_stmt(r)?);
            Stmt::Wait { sens, body }
        }
        16 => {
            let cond = if r.bool()? { Some(r_expr(r)?) } else { None };
            let n = r.u32()? as usize;
            if n > (1 << 16) {
                return Err("too many sens edges".to_string());
            }
            let mut sens = Vec::with_capacity(n);
            for _ in 0..n {
                sens.push(r_edge(r)?);
            }
            let timeout = if r.bool()? { Some(r_expr(r)?) } else { None };
            Stmt::WaitUntil {
                cond,
                sens,
                timeout,
            }
        }
        6 => {
            let (name, args, shapes) = r_call(r)?;
            Stmt::SysCall { name, args, shapes }
        }
        7 => Stmt::For {
            init: Box::new(r_stmt(r)?),
            cond: r_expr(r)?,
            step: Box::new(r_stmt(r)?),
            body: Box::new(r_stmt(r)?),
        },
        8 => Stmt::Repeat {
            count: r_expr(r)?,
            body: Box::new(r_stmt(r)?),
        },
        9 => Stmt::ExprStmt(r_expr(r)?),
        14 => Stmt::Return(Box::new(r_expr(r)?)),
        10 => Stmt::Break,
        11 => Stmt::Continue,
        12 => Stmt::Null,
        17 => Stmt::ProcContAssign {
            lhs: r_expr(r)?,
            rhs: r_expr(r)?,
        },
        18 => Stmt::Deassign { lhs: r_expr(r)? },
        13 => Stmt::Spanned {
            span: r_span(r)?,
            inner: Box::new(r_stmt(r)?),
        },
        t => return Err(format!("bad Stmt tag {}", t)),
    })
}
