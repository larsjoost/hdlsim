//! Checkpoint/restore: serialize the full dynamic simulator state to an own
//! binary format (`HDLCKPT1`) so a simulation can be stopped, saved, and
//! resumed later (`hdlsim run --checkpoint ck.bin` / `--restore ck.bin`).
//!
//! Only dynamic state is stored; the static design (`Flat`) is re-elaborated
//! on restore and checked against a fingerprint stored in the checkpoint, so
//! restoring against a different design fails loudly instead of corrupting.
//!
//! Child module of `sim` so it can read/write the kernel's private fields.

use super::{Ev, Frame, ProcRt, Simulator};
use hdl_ir::elab::Flat;
use hdl_ir::value::Value;
use hdl_ir::{BinOp, Edge, EdgeKind, Expr, Stmt, UnOp};
use hdl_syntax::source::Span;

const MAGIC: &[u8; 8] = b"HDLCKPT1";
const FORMAT_VERSION: u32 = 2;

// ---- little-endian writer/reader ----------------------------------------------

struct W<'a> {
    buf: &'a mut Vec<u8>,
}
impl<'a> W<'a> {
    fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    fn bool(&mut self, v: bool) {
        self.u8(v as u8);
    }
    fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn i64(&mut self, v: i64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn usize(&mut self, v: usize) {
        self.u64(v as u64);
    }
    fn str(&mut self, s: &str) {
        self.u32(s.len() as u32);
        self.buf.extend_from_slice(s.as_bytes());
    }
    fn value(&mut self, v: &Value) {
        self.u32(v.bits.len() as u32);
        self.buf.extend_from_slice(&v.bits);
    }
}

struct R<'a> {
    buf: &'a [u8],
    pos: usize,
}
impl<'a> R<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let s = self
            .buf
            .get(self.pos..self.pos.checked_add(n).ok_or("overflow")?)
            .ok_or("truncated checkpoint")?;
        self.pos += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn bool(&mut self) -> Result<bool, String> {
        Ok(self.u8()? != 0)
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().map_err(|_| "bad u32")?,
        ))
    }
    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().map_err(|_| "bad u64")?,
        ))
    }
    fn i64(&mut self) -> Result<i64, String> {
        Ok(i64::from_le_bytes(
            self.take(8)?.try_into().map_err(|_| "bad i64")?,
        ))
    }
    fn usize(&mut self) -> Result<usize, String> {
        Ok(self.u64()? as usize)
    }
    fn str(&mut self) -> Result<String, String> {
        let n = self.u32()? as usize;
        String::from_utf8(self.take(n)?.to_vec()).map_err(|_| "bad utf8".to_string())
    }
    fn value(&mut self) -> Result<Value, String> {
        let n = self.u32()? as usize;
        if n > (1 << 24) {
            return Err("value too large".to_string());
        }
        Ok(Value {
            bits: self.take(n)?.to_vec(),
        })
    }
}

// ---- enum tag maps (closed enums; the compiler checks exhaustiveness) ----------

fn w_binop(w: &mut W, op: BinOp) {
    w.u8(match op {
        BinOp::Add => 0,
        BinOp::Sub => 1,
        BinOp::Mul => 2,
        BinOp::Div => 3,
        BinOp::Mod => 4,
        BinOp::Shl => 5,
        BinOp::Shr => 6,
        BinOp::Sar => 7,
        BinOp::Lt => 8,
        BinOp::Le => 9,
        BinOp::Gt => 10,
        BinOp::Ge => 11,
        BinOp::Eq => 12,
        BinOp::Ne => 13,
        BinOp::EqStrict => 14,
        BinOp::NeStrict => 15,
        BinOp::BAnd => 16,
        BinOp::BOr => 17,
        BinOp::BXor => 18,
        BinOp::BNand => 19,
        BinOp::BNor => 20,
        BinOp::BXnor => 21,
        BinOp::LogAnd => 22,
        BinOp::LogOr => 23,
        BinOp::EqM => 24,
        BinOp::NeM => 25,
        BinOp::LtM => 26,
        BinOp::LeM => 27,
        BinOp::GtM => 28,
        BinOp::GeM => 29,
    })
}
fn r_binop(r: &mut R) -> Result<BinOp, String> {
    Ok(match r.u8()? {
        0 => BinOp::Add,
        1 => BinOp::Sub,
        2 => BinOp::Mul,
        3 => BinOp::Div,
        4 => BinOp::Mod,
        5 => BinOp::Shl,
        6 => BinOp::Shr,
        7 => BinOp::Sar,
        8 => BinOp::Lt,
        9 => BinOp::Le,
        10 => BinOp::Gt,
        11 => BinOp::Ge,
        12 => BinOp::Eq,
        13 => BinOp::Ne,
        14 => BinOp::EqStrict,
        15 => BinOp::NeStrict,
        16 => BinOp::BAnd,
        17 => BinOp::BOr,
        18 => BinOp::BXor,
        19 => BinOp::BNand,
        20 => BinOp::BNor,
        21 => BinOp::BXnor,
        22 => BinOp::LogAnd,
        23 => BinOp::LogOr,
        24 => BinOp::EqM,
        25 => BinOp::NeM,
        26 => BinOp::LtM,
        27 => BinOp::LeM,
        28 => BinOp::GtM,
        29 => BinOp::GeM,
        t => return Err(format!("bad BinOp tag {}", t)),
    })
}

fn w_unop(w: &mut W, op: UnOp) {
    w.u8(match op {
        UnOp::Not => 0,
        UnOp::BNot => 1,
        UnOp::Neg => 2,
        UnOp::Pos => 3,
        UnOp::RedAnd => 4,
        UnOp::RedOr => 5,
        UnOp::RedXor => 6,
        UnOp::RedNand => 7,
        UnOp::RedNor => 8,
        UnOp::RedXnor => 9,
    })
}
fn r_unop(r: &mut R) -> Result<UnOp, String> {
    Ok(match r.u8()? {
        0 => UnOp::Not,
        1 => UnOp::BNot,
        2 => UnOp::Neg,
        3 => UnOp::Pos,
        4 => UnOp::RedAnd,
        5 => UnOp::RedOr,
        6 => UnOp::RedXor,
        7 => UnOp::RedNand,
        8 => UnOp::RedNor,
        9 => UnOp::RedXnor,
        t => return Err(format!("bad UnOp tag {}", t)),
    })
}

fn w_edge_kind(w: &mut W, k: EdgeKind) {
    w.u8(match k {
        EdgeKind::Pos => 0,
        EdgeKind::Neg => 1,
        EdgeKind::Any => 2,
    })
}
fn r_edge_kind(r: &mut R) -> Result<EdgeKind, String> {
    Ok(match r.u8()? {
        0 => EdgeKind::Pos,
        1 => EdgeKind::Neg,
        2 => EdgeKind::Any,
        t => return Err(format!("bad EdgeKind tag {}", t)),
    })
}

fn w_edge(w: &mut W, e: &Edge) {
    w.u32(e.sig);
    w_edge_kind(w, e.kind);
}
fn r_edge(r: &mut R) -> Result<Edge, String> {
    Ok(Edge {
        sig: r.u32()?,
        kind: r_edge_kind(r)?,
    })
}

fn w_span(w: &mut W, s: &Span) {
    w.u32(s.file.0);
    w.u32(s.start);
    w.u32(s.end);
}
fn r_span(r: &mut R) -> Result<Span, String> {
    Ok(Span::new(
        hdl_syntax::source::FileId(r.u32()?),
        r.u32()?,
        r.u32()?,
    ))
}

// ---- Expr / Stmt / Frame --------------------------------------------------------

fn w_expr(w: &mut W, e: &Expr) {
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
        Expr::Call { name, args } => {
            w.u8(5);
            w.str(name);
            w.u32(args.len() as u32);
            for a in args {
                w_expr(w, a);
            }
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
        Expr::ToInt(e) => {
            w.u8(12);
            w_expr(w, e);
        }
        Expr::ToString(e) => {
            w.u8(13);
            w_expr(w, e);
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
    }
}

fn r_expr(r: &mut R) -> Result<Expr, String> {
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
            let name = r.str()?;
            let n = r.u32()? as usize;
            if n > (1 << 20) {
                return Err("too many args".to_string());
            }
            let mut args = Vec::with_capacity(n);
            for _ in 0..n {
                args.push(r_expr(r)?);
            }
            Expr::Call { name, args }
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
        12 => Expr::ToInt(Box::new(r_expr(r)?)),
        13 => Expr::ToString(Box::new(r_expr(r)?)),
        14 => Expr::Resize {
            e: Box::new(r_expr(r)?),
            width: r.usize()?,
            signed: r.bool()?,
        },
        15 => Expr::Signed(Box::new(r_expr(r)?)),
        16 => Expr::Real(f64::from_bits(r.u64()?)),
        t => return Err(format!("bad Expr tag {}", t)),
    })
}

fn w_stmt(w: &mut W, s: &Stmt) {
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
        Stmt::SysCall { name, args } => {
            w.u8(6);
            w.str(name);
            w.u32(args.len() as u32);
            for a in args {
                w_expr(w, a);
            }
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
        Stmt::Break => w.u8(10),
        Stmt::Continue => w.u8(11),
        Stmt::Null => w.u8(12),
        Stmt::Spanned { span, inner } => {
            w.u8(13);
            w_span(w, span);
            w_stmt(w, inner);
        }
    }
}

fn r_stmt(r: &mut R) -> Result<Stmt, String> {
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
        6 => {
            let name = r.str()?;
            let n = r.u32()? as usize;
            if n > (1 << 20) {
                return Err("too many args".to_string());
            }
            let mut args = Vec::with_capacity(n);
            for _ in 0..n {
                args.push(r_expr(r)?);
            }
            Stmt::SysCall { name, args }
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
        10 => Stmt::Break,
        11 => Stmt::Continue,
        12 => Stmt::Null,
        13 => Stmt::Spanned {
            span: r_span(r)?,
            inner: Box::new(r_stmt(r)?),
        },
        t => return Err(format!("bad Stmt tag {}", t)),
    })
}

fn w_frame(w: &mut W, f: &Frame) {
    match f {
        Frame::Block { block, pc } => {
            w.u8(0);
            w.u32(block.len() as u32);
            for s in block {
                w_stmt(w, s);
            }
            w.usize(*pc);
        }
        Frame::ForIter {
            cond,
            step,
            body,
            after_body,
        } => {
            w.u8(1);
            w_expr(w, cond);
            w_stmt(w, step);
            w_stmt(w, body);
            w.bool(*after_body);
        }
        Frame::RepeatIter {
            remaining,
            body,
            after_body,
        } => {
            w.u8(2);
            w.i64(*remaining);
            w_stmt(w, body);
            w.bool(*after_body);
        }
    }
}

fn r_frame(r: &mut R) -> Result<Frame, String> {
    Ok(match r.u8()? {
        0 => {
            let n = r.u32()? as usize;
            if n > (1 << 20) {
                return Err("too many frame stmts".to_string());
            }
            let mut block = Vec::with_capacity(n);
            for _ in 0..n {
                block.push(r_stmt(r)?);
            }
            Frame::Block {
                block,
                pc: r.usize()?,
            }
        }
        1 => Frame::ForIter {
            cond: r_expr(r)?,
            step: Box::new(r_stmt(r)?),
            body: Box::new(r_stmt(r)?),
            after_body: r.bool()?,
        },
        2 => Frame::RepeatIter {
            remaining: r.i64()?,
            body: Box::new(r_stmt(r)?),
            after_body: r.bool()?,
        },
        t => return Err(format!("bad Frame tag {}", t)),
    })
}

fn w_ev(w: &mut W, e: &Ev) {
    match e {
        Ev::Resume(pid) => {
            w.u8(0);
            w.u32(*pid);
        }
    }
}
fn r_ev(r: &mut R) -> Result<Ev, String> {
    match r.u8()? {
        0 => Ok(Ev::Resume(r.u32()?)),
        t => Err(format!("bad Ev tag {}", t)),
    }
}

// ---- fingerprint ----------------------------------------------------------------

/// FNV-1a over the static design shape (signal names/widths/mems, proc
/// origins/kinds, function names, assign count) so a checkpoint refuses to
/// restore against a different design.
fn fingerprint(flat: &Flat) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    let mut mix = |bytes: &[u8]| {
        for b in bytes {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
    };
    for s in &flat.signals {
        mix(s.name.as_bytes());
        mix(&s.width.to_le_bytes());
        mix(&[s.mem.is_some() as u8, s.signed as u8]);
    }
    for p in &flat.procs {
        mix(p.origin.as_bytes());
        mix(&[match p.kind {
            hdl_ir::ProcKind::Always => 0,
            hdl_ir::ProcKind::Initial => 1,
        }]);
    }
    for f in &flat.functions {
        mix(f.name.as_bytes());
    }
    mix(&flat.assigns.len().to_le_bytes());
    h
}

// ---- save / load ------------------------------------------------------------------

pub(crate) fn save(sim: &Simulator, out: &mut Vec<u8>) {
    out.extend_from_slice(MAGIC);
    let mut w = W { buf: out };
    w.u32(FORMAT_VERSION);
    w.u64(fingerprint(&sim.flat));
    w.u32(sim.flat.signals.len() as u32);
    w.u32(sim.flat.procs.len() as u32);
    w.u64(sim.time);
    w.u32(sim.vals.len() as u32);
    for v in &sim.vals {
        w.value(v);
    }
    w.u32(sim.prev.len() as u32);
    for v in &sim.prev {
        w.value(v);
    }
    w.u32(sim.mems.len() as u32);
    for m in &sim.mems {
        w.u32(m.len() as u32);
        for v in m {
            w.value(v);
        }
    }
    w.u32(sim.active.len() as u32);
    for p in &sim.active {
        w.u32(*p);
    }
    w.u32(sim.nba.len() as u32);
    for (sig, v, drv) in &sim.nba {
        w.u32(*sig);
        w.value(v);
        w.bool(drv.is_some());
        if let Some(d) = drv {
            w.u32(*d);
        }
    }
    w.u32(sim.nba_exprs.len() as u32);
    for (lhs, v, drv) in &sim.nba_exprs {
        w_expr(&mut w, lhs);
        w.value(v);
        w.bool(drv.is_some());
        if let Some(d) = drv {
            w.u32(*d);
        }
    }
    w.u32(sim.time_events.len() as u32);
    for (t, evs) in &sim.time_events {
        w.u64(*t);
        w.u32(evs.len() as u32);
        for e in evs {
            w_ev(&mut w, e);
        }
    }
    w.u32(sim.proc_rt.len() as u32);
    for rt in &sim.proc_rt {
        w_proc_rt(&mut w, rt);
    }
    w.u32(sim.waiting.len() as u32);
    for (pid, edges) in &sim.waiting {
        w.u32(*pid);
        w.u32(edges.len() as u32);
        for (sig, kind) in edges {
            w.u32(*sig);
            w_edge_kind(&mut w, *kind);
        }
    }
    w.bool(sim.finished);
    w.bool(sim.stopped);
    w.bool(sim.started);
    w.bool(sim.finish_notified);
    w.u64(sim.failures);
    w.u32(sim.monitor.len() as u32);
    for exprs in &sim.monitor {
        w.u32(exprs.len() as u32);
        for e in exprs {
            w_expr(&mut w, e);
        }
    }
    w.u32(sim.monitor_last.len() as u32);
    for last in &sim.monitor_last {
        w.bool(last.is_some());
        if let Some(vs) = last {
            w.u32(vs.len() as u32);
            for v in vs {
                w.value(v);
            }
        }
    }
    w.u32(sim.delta_guard);
    w.u64(sim.rng.get());
    w.bool(sim.cur_proc.is_some());
    if let Some(p) = sim.cur_proc {
        w.u32(p);
    }
    w.u32(sim.last_driver.len() as u32);
    for d in &sim.last_driver {
        w.bool(d.is_some());
        if let Some((p, t)) = d {
            w.u32(*p);
            w.u64(*t);
        }
    }
}

fn w_proc_rt(w: &mut W, rt: &ProcRt) {
    w.u32(rt.frames.len() as u32);
    for f in &rt.frames {
        w_frame(w, f);
    }
}

pub(crate) fn load(flat: Flat, bytes: &[u8]) -> Result<Simulator, String> {
    if bytes.get(..8) != Some(MAGIC) {
        return Err("not an hdlsim checkpoint (bad magic)".to_string());
    }
    let mut r = R { buf: bytes, pos: 8 };
    let version = r.u32()?;
    if version != FORMAT_VERSION {
        return Err(format!("unsupported checkpoint version {}", version));
    }
    let fp = r.u64()?;
    if fp != fingerprint(&flat) {
        return Err(
            "checkpoint does not match this design (signal/process fingerprint differs)"
                .to_string(),
        );
    }
    let n_sig = r.u32()? as usize;
    let n_proc = r.u32()? as usize;
    if n_sig != flat.signals.len() || n_proc != flat.procs.len() {
        return Err("checkpoint design shape differs".to_string());
    }
    let mut sim = Simulator::new(flat);
    sim.time = r.u64()?;
    let n = r.u32()? as usize;
    if n != sim.vals.len() {
        return Err("signal count mismatch".to_string());
    }
    sim.vals.clear();
    for _ in 0..n {
        sim.vals.push(r.value()?);
    }
    let n = r.u32()? as usize;
    if n != sim.prev.len() {
        return Err("prev count mismatch".to_string());
    }
    sim.prev.clear();
    for _ in 0..n {
        sim.prev.push(r.value()?);
    }
    let n = r.u32()? as usize;
    if n != sim.mems.len() {
        return Err("mem count mismatch".to_string());
    }
    sim.mems.clear();
    for _ in 0..n {
        let m = r.u32()? as usize;
        if m > (1 << 24) {
            return Err("memory too large".to_string());
        }
        let mut mem = Vec::with_capacity(m);
        for _ in 0..m {
            mem.push(r.value()?);
        }
        sim.mems.push(mem);
    }
    let n = r.u32()? as usize;
    if n > (1 << 24) {
        return Err("active queue too large".to_string());
    }
    sim.active.clear();
    for _ in 0..n {
        sim.active.push_back(r.u32()?);
    }
    let n = r.u32()? as usize;
    if n > (1 << 24) {
        return Err("nba too large".to_string());
    }
    sim.nba.clear();
    for _ in 0..n {
        let sig = r.u32()?;
        let v = r.value()?;
        let drv = if r.bool()? { Some(r.u32()?) } else { None };
        sim.nba.push((sig, v, drv));
    }
    let n = r.u32()? as usize;
    if n > (1 << 24) {
        return Err("nba_exprs too large".to_string());
    }
    sim.nba_exprs.clear();
    for _ in 0..n {
        let lhs = r_expr(&mut r)?;
        let v = r.value()?;
        let drv = if r.bool()? { Some(r.u32()?) } else { None };
        sim.nba_exprs.push((lhs, v, drv));
    }
    let n = r.u32()? as usize;
    if n > (1 << 24) {
        return Err("time_events too large".to_string());
    }
    sim.time_events.clear();
    for _ in 0..n {
        let t = r.u64()?;
        let m = r.u32()? as usize;
        if m > (1 << 24) {
            return Err("too many events".to_string());
        }
        let mut evs = Vec::with_capacity(m);
        for _ in 0..m {
            evs.push(r_ev(&mut r)?);
        }
        sim.time_events.insert(t, evs);
    }
    let n = r.u32()? as usize;
    if n != sim.proc_rt.len() {
        return Err("proc count mismatch".to_string());
    }
    sim.proc_rt.clear();
    for _ in 0..n {
        let m = r.u32()? as usize;
        if m > (1 << 20) {
            return Err("too many frames".to_string());
        }
        let mut frames = Vec::with_capacity(m);
        for _ in 0..m {
            frames.push(r_frame(&mut r)?);
        }
        sim.proc_rt.push(ProcRt { frames });
    }
    let n = r.u32()? as usize;
    if n > (1 << 24) {
        return Err("waiting too large".to_string());
    }
    sim.waiting.clear();
    for _ in 0..n {
        let pid = r.u32()?;
        let m = r.u32()? as usize;
        if m > (1 << 16) {
            return Err("too many edges".to_string());
        }
        let mut edges = Vec::with_capacity(m);
        for _ in 0..m {
            edges.push((r.u32()?, r_edge_kind(&mut r)?));
        }
        sim.waiting.push((pid, edges));
    }
    sim.finished = r.bool()?;
    sim.stopped = r.bool()?;
    sim.started = r.bool()?;
    sim.finish_notified = r.bool()?;
    sim.failures = r.u64()?;
    let n = r.u32()? as usize;
    if n > (1 << 24) {
        return Err("monitor too large".to_string());
    }
    sim.monitor.clear();
    for _ in 0..n {
        let m = r.u32()? as usize;
        if m > (1 << 20) {
            return Err("too many monitor exprs".to_string());
        }
        let mut exprs = Vec::with_capacity(m);
        for _ in 0..m {
            exprs.push(r_expr(&mut r)?);
        }
        sim.monitor.push(exprs);
    }
    let n = r.u32()? as usize;
    if n > (1 << 24) {
        return Err("monitor_last too large".to_string());
    }
    sim.monitor_last.clear();
    for _ in 0..n {
        if r.bool()? {
            let m = r.u32()? as usize;
            if m > (1 << 20) {
                return Err("too many monitor values".to_string());
            }
            let mut vs = Vec::with_capacity(m);
            for _ in 0..m {
                vs.push(r.value()?);
            }
            sim.monitor_last.push(Some(vs));
        } else {
            sim.monitor_last.push(None);
        }
    }
    sim.delta_guard = r.u32()?;
    sim.rng.set(r.u64()?);
    sim.cur_proc = if r.bool()? { Some(r.u32()?) } else { None };
    let n = r.u32()? as usize;
    if n != sim.last_driver.len() {
        return Err("last_driver count mismatch".to_string());
    }
    sim.last_driver.clear();
    for _ in 0..n {
        if r.bool()? {
            let p = r.u32()?;
            let t = r.u64()?;
            sim.last_driver.push(Some((p, t)));
        } else {
            sim.last_driver.push(None);
        }
    }
    if r.pos != bytes.len() {
        return Err("trailing bytes in checkpoint".to_string());
    }
    Ok(sim)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hdl_ir::value::V1;

    fn roundtrip_expr(e: &Expr) -> Expr {
        let mut buf = Vec::new();
        w_expr(&mut W { buf: &mut buf }, e);
        let mut r = R { buf: &buf, pos: 0 };
        r_expr(&mut r).unwrap()
    }

    fn roundtrip_stmt(s: &Stmt) -> Stmt {
        let mut buf = Vec::new();
        w_stmt(&mut W { buf: &mut buf }, s);
        let mut r = R { buf: &buf, pos: 0 };
        r_stmt(&mut r).unwrap()
    }

    #[test]
    fn expr_roundtrip_all_variants() {
        let sig = |i: u32| Expr::Sig(i);
        let cases = vec![
            Expr::Lit(Value::from_u64(8, 0xA5)),
            sig(3),
            Expr::Bin {
                op: BinOp::Add,
                l: Box::new(sig(1)),
                r: Box::new(Expr::Lit(Value::from_u64(8, 1))),
            },
            Expr::Un {
                op: UnOp::BNot,
                e: Box::new(sig(2)),
            },
            Expr::Cond {
                c: Box::new(sig(0)),
                t: Box::new(sig(1)),
                e: Box::new(sig(2)),
            },
            Expr::Call {
                name: "to_integer".to_string(),
                args: vec![sig(4)],
            },
            Expr::Concat(vec![sig(1), sig(2)]),
            Expr::Replicate {
                count: Box::new(Expr::Lit(Value::from_u64(32, 4))),
                e: Box::new(sig(1)),
            },
            Expr::Sel {
                e: Box::new(sig(9)),
                msb: Box::new(Expr::Lit(Value::from_u64(32, 7))),
                lsb: Box::new(Expr::Lit(Value::from_u64(32, 0))),
            },
            Expr::Idx {
                e: Box::new(sig(5)),
                i: Box::new(sig(6)),
            },
            Expr::Rising(Box::new(sig(0))),
            Expr::Falling(Box::new(sig(0))),
            Expr::ToInt(Box::new(sig(7))),
            Expr::ToString(Box::new(sig(7))),
            Expr::Resize {
                e: Box::new(sig(8)),
                width: 16,
                signed: true,
            },
            Expr::Signed(Box::new(sig(8))),
        ];
        for c in cases {
            let back = roundtrip_expr(&c);
            assert_eq!(format!("{:?}", back), format!("{:?}", c));
        }
    }

    #[test]
    fn stmt_roundtrip_all_variants() {
        let a = Stmt::Assign {
            lhs: Expr::Sig(1),
            rhs: Expr::Bin {
                op: BinOp::Add,
                l: Box::new(Expr::Sig(1)),
                r: Box::new(Expr::Lit(Value::from_u64(8, 1))),
            },
            nba: true,
        };
        let cases = vec![
            Stmt::Block(vec![a.clone(), Stmt::Break, Stmt::Continue, Stmt::Null]),
            Stmt::If {
                cond: Expr::Sig(0),
                then: Box::new(a.clone()),
                els: Some(Box::new(Stmt::Null)),
            },
            Stmt::Case {
                expr: Expr::Sig(2),
                items: vec![(vec![Expr::Lit(Value { bits: vec![V1] })], a.clone())],
                default: Some(Box::new(Stmt::Null)),
                matching: false,
            },
            a.clone(),
            Stmt::Delay {
                time_fs: 1000,
                stmt: Box::new(a.clone()),
            },
            Stmt::Wait {
                sens: vec![Edge {
                    sig: 0,
                    kind: EdgeKind::Pos,
                }],
                body: Box::new(a.clone()),
            },
            Stmt::SysCall {
                name: "$display".to_string(),
                args: vec![Expr::Sig(3)],
            },
            Stmt::For {
                init: Box::new(a.clone()),
                cond: Expr::Sig(0),
                step: Box::new(a.clone()),
                body: Box::new(a.clone()),
            },
            Stmt::Repeat {
                count: Expr::Lit(Value::from_u64(32, 3)),
                body: Box::new(a.clone()),
            },
            Stmt::ExprStmt(Expr::Sig(7)),
            Stmt::Spanned {
                span: Span::new(hdl_syntax::source::FileId(2), 10, 20),
                inner: Box::new(a.clone()),
            },
        ];
        for c in cases {
            let back = roundtrip_stmt(&c);
            assert_eq!(format!("{:?}", back), format!("{:?}", c));
        }
    }

    #[test]
    fn frame_roundtrip() {
        let body = Stmt::Assign {
            lhs: Expr::Sig(1),
            rhs: Expr::Lit(Value::from_u64(8, 1)),
            nba: false,
        };
        let frames = vec![
            Frame::Block {
                block: vec![body.clone()],
                pc: 1,
            },
            Frame::ForIter {
                cond: Expr::Sig(0),
                step: Box::new(body.clone()),
                body: Box::new(body.clone()),
                after_body: true,
            },
            Frame::RepeatIter {
                remaining: 5,
                body: Box::new(body.clone()),
                after_body: false,
            },
        ];
        let mut buf = Vec::new();
        {
            let mut w = W { buf: &mut buf };
            for f in &frames {
                w_frame(&mut w, f);
            }
        }
        let mut r = R { buf: &buf, pos: 0 };
        for f in &frames {
            let back = r_frame(&mut r).unwrap();
            assert_eq!(format!("{:?}", back), format!("{:?}", f));
        }
    }

    fn empty_flat() -> Flat {
        Flat {
            signals: Vec::new(),
            procs: Vec::new(),
            assigns: Vec::new(),
            functions: Vec::new(),
            scopes: Vec::new(),
            top_scope: 0,
        }
    }

    #[test]
    fn load_rejects_bad_magic_and_trailing_bytes() {
        let flat = empty_flat();
        assert!(load(flat.clone(), b"NOPE1234").is_err());
        let mut buf = Vec::new();
        save(&Simulator::new(empty_flat()), &mut buf);
        buf.push(0);
        assert!(load(flat, &buf).is_err());
    }

    #[test]
    fn empty_simulator_save_load_roundtrip() {
        let mut buf = Vec::new();
        save(&Simulator::new(empty_flat()), &mut buf);
        assert_eq!(&buf[..8], MAGIC);
        let sim = load(empty_flat(), &buf).unwrap();
        assert_eq!(sim.time(), 0);
        assert_eq!(sim.failures(), 0);
    }
}
