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
use hdl_ir::{Edge, EdgeKind, Expr, ShapeBind, Stmt};
use hdl_syntax::source::Span;

const MAGIC: &[u8; 8] = b"HDLCKPT1";
const FORMAT_VERSION: u32 = 6;

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
            real: false,
        })
    }
}

mod ir;
mod op;

use ir::{r_expr, r_stmt, w_expr, w_stmt};
use op::{r_binop, r_unop, w_binop, w_unop};

// ---- enum tag maps (closed enums; the compiler checks exhaustiveness) ----------

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
    w.bool(e.iff.is_some());
    if let Some(c) = &e.iff {
        w_expr(w, c);
    }
}
fn r_edge(r: &mut R) -> Result<Edge, String> {
    let sig = r.u32()?;
    let kind = r_edge_kind(r)?;
    let iff = if r.bool()? { Some(r_expr(r)?) } else { None };
    Ok(Edge { sig, kind, iff })
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
        Frame::WaitUntilCheck {
            cond,
            edges,
            deadline,
            gen,
        } => {
            w.u8(3);
            match cond {
                Some(c) => {
                    w.bool(true);
                    w_expr(w, c);
                }
                None => w.bool(false),
            }
            w.u32(edges.len() as u32);
            for e in edges {
                w_edge(w, e);
            }
            w.u64(*deadline);
            w.u64(*gen);
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
        3 => {
            let cond = if r.bool()? { Some(r_expr(r)?) } else { None };
            let n = r.u32()? as usize;
            if n > (1 << 16) {
                return Err("too many wait edges".to_string());
            }
            let mut edges = Vec::with_capacity(n);
            for _ in 0..n {
                edges.push(r_edge(r)?);
            }
            Frame::WaitUntilCheck {
                cond,
                edges,
                deadline: r.u64()?,
                gen: r.u64()?,
            }
        }
        t => return Err(format!("bad Frame tag {}", t)),
    })
}

fn w_ev(w: &mut W, e: &Ev) {
    match e {
        Ev::Resume(pid) => {
            w.u8(0);
            w.u32(*pid);
        }
        Ev::Nba(sig, v, drv) => {
            w.u8(1);
            w.u32(*sig);
            w.value(v);
            w.bool(drv.is_some());
            if let Some(d) = drv {
                w.u32(*d);
            }
        }
        Ev::NbaExpr(lhs, v, drv) => {
            w.u8(2);
            w_expr(w, lhs);
            w.value(v);
            w.bool(drv.is_some());
            if let Some(d) = drv {
                w.u32(*d);
            }
        }
        Ev::WaitTimeout(pid, gen) => {
            w.u8(3);
            w.u32(*pid);
            w.u64(*gen);
        }
    }
}
fn r_ev(r: &mut R) -> Result<Ev, String> {
    match r.u8()? {
        0 => Ok(Ev::Resume(r.u32()?)),
        3 => Ok(Ev::WaitTimeout(r.u32()?, r.u64()?)),
        1 => {
            let sig = r.u32()?;
            let v = r.value()?;
            let drv = if r.bool()? { Some(r.u32()?) } else { None };
            Ok(Ev::Nba(sig, v, drv))
        }
        2 => {
            let lhs = r_expr(r)?;
            let v = r.value()?;
            let drv = if r.bool()? { Some(r.u32()?) } else { None };
            Ok(Ev::NbaExpr(lhs, v, drv))
        }
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
    w.u64(sim.delta_epoch);
    w.u32(sim.sig_evt.len() as u32);
    for e in &sim.sig_evt {
        w.u64(*e);
    }
    // Active procedural continuous assignments: (target, lhs, rhs).
    w.u32(sim.proc_assigns.len() as u32);
    for (_, lhs, rhs) in &sim.proc_assigns {
        w_expr(&mut w, lhs);
        w_expr(&mut w, rhs);
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
    // Parked processes: pid + the wait's edges (their `cur_sens`).
    let parked_list: Vec<(u32, &Vec<Edge>)> = (0..sim.parked.len())
        .filter(|&i| sim.parked[i])
        .map(|i| (i as u32, &sim.cur_sens[i]))
        .collect();
    w.u32(parked_list.len() as u32);
    for (pid, edges) in parked_list {
        w.u32(pid);
        w.u32(edges.len() as u32);
        for e in edges {
            w_edge(&mut w, e);
        }
    }
    w.bool(sim.finished);
    w.bool(sim.stopped);
    w.bool(sim.started);
    w.bool(sim.finish_notified);
    w.bool(sim.finish_status.is_some());
    if let Some(s) = sim.finish_status {
        w.u32(s as u32);
    }
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
    // Refuse runaway allocations with the top-allocator report instead of
    // aborting inside the first big `vec![…]`.
    Simulator::alloc_report(&flat)?;
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
    sim.delta_epoch = r.u64()?;
    let n = r.u32()? as usize;
    if n != sim.sig_evt.len() {
        return Err("sig_evt count mismatch".to_string());
    }
    sim.sig_evt.clear();
    for _ in 0..n {
        sim.sig_evt.push(r.u64()?);
    }
    let n = r.u32()? as usize;
    if n > (1 << 16) {
        return Err("too many proc assigns".to_string());
    }
    sim.proc_assigns.clear();
    for _ in 0..n {
        let lhs = r_expr(&mut r)?;
        let rhs = r_expr(&mut r)?;
        let sig =
            crate::sim::lhs_sig(&lhs).ok_or("proc assign target must be a simple variable")?;
        sim.proc_assigns.push((sig, lhs, rhs));
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
        let p = r.u32()?;
        sim.active.push_back(p);
        sim.in_active[p as usize] = true;
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
    for _ in 0..n {
        let pid = r.u32()?;
        let m = r.u32()? as usize;
        if m > (1 << 16) {
            return Err("too many edges".to_string());
        }
        let mut edges = Vec::with_capacity(m);
        for _ in 0..m {
            edges.push(r_edge(&mut r)?);
        }
        // Re-park: swaps the process's `sens_index` entries to the wait's
        // edges, exactly as the original park did.
        sim.park(pid, edges);
    }
    sim.finished = r.bool()?;
    sim.stopped = r.bool()?;
    sim.started = r.bool()?;
    sim.finish_notified = r.bool()?;
    sim.finish_status = if r.bool()? {
        Some(r.u32()? as i32)
    } else {
        None
    };
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
    use hdl_ir::{BinOp, UnOp};

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
                shapes: vec![ShapeBind {
                    formal: 0,
                    lo: sig(6),
                    asc: sig(7),
                }],
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
            Expr::ToString {
                e: Box::new(sig(7)),
                boolean: true,
            },
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
                items: vec![(
                    vec![Expr::Lit(Value {
                        bits: vec![V1],
                        real: false,
                    })],
                    a.clone(),
                )],
                default: Some(Box::new(Stmt::Null)),
                matching: false,
            },
            a.clone(),
            Stmt::AssignDelayed {
                lhs: Expr::Sig(1),
                rhs: Expr::Sig(2),
                delay: Expr::Lit(Value::from_u64(64, 5000)),
            },
            Stmt::Delay {
                time_fs: 1000,
                stmt: Box::new(a.clone()),
            },
            Stmt::Wait {
                sens: vec![Edge {
                    sig: 0,
                    kind: EdgeKind::Pos,
                    iff: None,
                }],
                body: Box::new(a.clone()),
            },
            Stmt::SysCall {
                name: "$display".to_string(),
                args: vec![Expr::Sig(3)],
                shapes: vec![ShapeBind {
                    formal: 0,
                    lo: Expr::Lit(Value::from_u64(32, 1)),
                    asc: Expr::Lit(Value::from_u64(32, 0)),
                }],
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
