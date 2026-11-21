//! FST sink — writes `.fst` (GTKWave's Fast Signal Trace format) via
//! `run --fst out.fst`. Readable by GTKWave and by `hdlsim wave out.fst`.
//!
//! The whole timeline is buffered and emitted as a single value-change block
//! at `finish` (FST sections are self-delimiting and seekable, but building
//! them incrementally needs the same machinery; v1 trades memory for
//! simplicity). Layout: header block, one VC block (bits array + per-variable
//! record chains + position table + time table), geometry block, gzip'd
//! hierarchy block. All compressed sub-streams use stored deflate blocks (see
//! [`crate::fst`]), so the writer needs no compression crate.

use super::WaveSink;
use crate::fst::{gzip_store, put_cstr, put_u64be, put_varint, zlib_store};
use hdl_ir::elab::Flat;
use hdl_ir::value::{Value, V0, V1, VDASH, VH, VL, VU, VW, VX, VZ};
use hdl_sim::events::SimEvent;
use std::collections::BTreeMap;
use std::io::Write;

const WRITER_STR: &str = concat!("hdlsim ", env!("CARGO_PKG_VERSION"));
const TIMESCALE_FS: i8 = -15;

/// Buffered `.fst` writer.
pub struct FstSink<W: Write> {
    w: W,
    events: Vec<SimEvent>,
}

impl<W: Write> FstSink<W> {
    pub fn new(w: W, _flat: &Flat) -> FstSink<W> {
        FstSink {
            w,
            events: Vec::new(),
        }
    }
}

impl<W: Write> WaveSink for FstSink<W> {
    fn on_event(&mut self, ev: &SimEvent, _flat: &Flat) {
        match ev {
            SimEvent::TimeAdvance { .. }
            | SimEvent::SignalChange { .. }
            | SimEvent::Finish { .. }
            | SimEvent::Stop { .. } => self.events.push(ev.clone()),
            _ => {} // messages/driver/proc events have no FST representation
        }
    }
    fn finish(&mut self, flat: &Flat) {
        let bytes = build_fst(flat, &self.events);
        let _ = self.w.write_all(&bytes);
        let _ = self.w.flush();
    }
}

/// One bit as an FST value char.
fn bit_char(b: u8) -> u8 {
    match b {
        V0 => b'0',
        V1 => b'1',
        VZ => b'z',
        VH => b'h',
        VU => b'u',
        VW => b'w',
        VL => b'l',
        VDASH => b'-',
        _ => b'x',
    }
}

/// Value as an MSB-first FST char string of exactly `w` chars.
fn fst_chars(v: &Value, w: usize) -> Vec<u8> {
    (0..w)
        .rev()
        .map(|i| bit_char(v.bits.get(i).copied().unwrap_or(VX)))
        .collect()
}

/// FST_RCV code for a non-0/1 single-bit char.
fn rcv_code(c: u8) -> u64 {
    match c {
        b'z' | b'Z' => 3,
        b'h' | b'H' => 5,
        b'u' | b'U' => 7,
        b'w' | b'W' => 9,
        b'l' | b'L' => 11,
        b'-' => 13,
        _ => 1, // x
    }
}

fn event_time(ev: &SimEvent) -> Option<u64> {
    match ev {
        SimEvent::TimeAdvance { time_fs }
        | SimEvent::SignalChange { time_fs, .. }
        | SimEvent::Finish { time_fs }
        | SimEvent::Stop { time_fs } => Some(*time_fs),
        _ => None,
    }
}

/// Serialize the whole trace as an FST file image.
fn build_fst(flat: &Flat, events: &[SimEvent]) -> Vec<u8> {
    let n = flat.signals.len();

    // Handle order = hierarchy emission order (pre-order scope walk, signals in
    // flat order within each scope). Everything (bits, records, geom, hier) is
    // indexed by handle = position in this order + 1.
    fn scope_paths(scopes: &[hdl_ir::elab::Scope]) -> Vec<String> {
        fn path_of(scopes: &[hdl_ir::elab::Scope], paths: &[String], id: usize) -> String {
            let s = &scopes[id];
            match s.parent {
                None => s.name.clone(),
                Some(p) => {
                    let pp = &paths[p as usize];
                    if pp.is_empty() {
                        s.name.clone()
                    } else {
                        format!("{}.{}", pp, s.name)
                    }
                }
            }
        }
        let mut paths: Vec<String> = Vec::with_capacity(scopes.len());
        for i in 0..scopes.len() {
            let p = path_of(scopes, &paths, i);
            paths.push(p);
        }
        paths
    }
    let scope_path = scope_paths(&flat.scopes);
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); flat.scopes.len()];
    for (i, s) in flat.scopes.iter().enumerate() {
        if let Some(p) = s.parent {
            children[p as usize].push(i);
        }
    }
    let mut order: Vec<usize> = Vec::with_capacity(n);
    fn emit_scope(
        id: usize,
        flat: &Flat,
        children: &Vec<Vec<usize>>,
        order: &mut Vec<usize>,
        emitted: &mut Vec<usize>,
    ) {
        emitted.push(id);
        for (i, sig) in flat.signals.iter().enumerate() {
            if sig.scope as usize == id {
                order.push(i);
            }
        }
        for &ch in &children[id] {
            emit_scope(ch, flat, children, order, emitted);
        }
    }
    let mut emitted_scopes: Vec<usize> = Vec::new();
    if !flat.scopes.is_empty() {
        emit_scope(
            flat.top_scope as usize,
            flat,
            &children,
            &mut order,
            &mut emitted_scopes,
        );
    }
    // Signals in unreachable scopes (shouldn't happen) still get handles.
    for i in 0..n {
        if !order.contains(&i) {
            order.push(i);
        }
    }
    let handle_of: Vec<usize> = {
        let mut h = vec![0usize; n];
        for (pos, &sig) in order.iter().enumerate() {
            h[sig] = pos;
        }
        h
    };
    let widths: Vec<usize> = order
        .iter()
        .map(|&i| flat.signals[i].width.max(1))
        .collect();
    // Real signals: FST stores them as FST_VT_VCD_REAL with 8 raw f64 bytes
    // (geom width 0), not as bit chars.
    let reals: Vec<bool> = order
        .iter()
        .map(|&i| flat.signals[i].kind == hdl_ir::SigKind::Real)
        .collect();

    // Time table: sorted unique event times.
    let mut tindex: BTreeMap<u64, usize> = BTreeMap::new();
    for ev in events {
        if let Some(t) = event_time(ev) {
            if !tindex.contains_key(&t) {
                let idx = tindex.len();
                tindex.insert(t, idx);
            }
        }
    }
    let times: Vec<u64> = tindex.keys().copied().collect();
    let times = if times.is_empty() { vec![0u64] } else { times };
    let start = times[0];
    let end = *times.last().unwrap();

    // Values at the section start time + record chains per handle.
    let mut cur: Vec<Value> = order
        .iter()
        .map(|&i| {
            flat.signals[i]
                .init
                .clone()
                .unwrap_or_else(|| Value::x(flat.signals[i].width.max(1)))
        })
        .collect();
    let mut records: Vec<Vec<(usize, Value)>> = vec![Vec::new(); n];
    let mut prev_ti: Vec<usize> = vec![0; n];
    for ev in events {
        if let SimEvent::SignalChange {
            sig, new, time_fs, ..
        } = ev
        {
            let h = handle_of[*sig as usize];
            let ti = tindex[time_fs];
            if ti == 0 {
                cur[h] = new.clone();
            } else {
                records[h].push((ti - prev_ti[h], new.clone()));
                prev_ti[h] = ti;
            }
        }
    }

    // Bits array: initial values, MSB-first chars per variable.
    let mut bits: Vec<u8> = Vec::new();
    for h in 0..n {
        if reals[h] {
            let f = cur[h].to_f64().unwrap_or(f64::NAN);
            bits.extend_from_slice(&f.to_le_bytes());
        } else {
            bits.extend_from_slice(&fst_chars(&cur[h], widths[h]));
        }
    }

    // Record chains.
    let mut rec_bytes: Vec<Vec<u8>> = Vec::with_capacity(n);
    for h in 0..n {
        let mut rb = Vec::new();
        for (td, val) in &records[h] {
            if reals[h] {
                let f = val.to_f64().unwrap_or(f64::NAN);
                put_varint(&mut rb, ((*td as u64) << 1) | 1);
                rb.extend_from_slice(&f.to_le_bytes());
                continue;
            }
            let chars = fst_chars(val, widths[h]);
            if widths[h] == 1 {
                let c = chars[0];
                let rcv = match c {
                    b'0' => (*td as u64) << 2,
                    b'1' => ((*td as u64) << 2) | 2,
                    _ => rcv_code(c) | ((*td as u64) << 4),
                };
                put_varint(&mut rb, rcv);
            } else if chars.iter().all(|&c| c == b'0' || c == b'1') {
                put_varint(&mut rb, (*td as u64) << 1);
                for j in 0..(widths[h] + 7) / 8 {
                    let mut acc = 0u8;
                    for k in 0..8 {
                        let ci = 8 * j + k;
                        if ci < widths[h] && chars[ci] == b'1' {
                            acc |= 1 << (7 - k);
                        }
                    }
                    rb.push(acc);
                }
            } else {
                put_varint(&mut rb, ((*td as u64) << 1) | 1);
                rb.extend_from_slice(&chars);
            }
        }
        rec_bytes.push(rb);
    }

    // ---- VC block ----
    let mut vc: Vec<u8> = Vec::new();
    put_u64be(&mut vc, start);
    put_u64be(&mut vc, end);
    let memreq: u64 = rec_bytes.iter().map(|r| r.len() as u64 + 1).sum();
    put_u64be(&mut vc, memreq);
    let bits_z = zlib_store(&bits);
    put_varint(&mut vc, bits.len() as u64);
    put_varint(&mut vc, bits_z.len() as u64);
    put_varint(&mut vc, n as u64);
    vc.extend_from_slice(&bits_z);
    put_varint(&mut vc, n as u64); // waves count
    vc.push(b'Z'); // packtype: zlib
    let mut fpos = 1u64; // record positions are 1-based relative to packtype
    let mut pos_table: Vec<u8> = Vec::new();
    let mut prevpos = 0u64;
    let mut zerocnt = 0u64;
    for rb in &rec_bytes {
        if rb.is_empty() {
            zerocnt += 1;
            continue;
        }
        if zerocnt > 0 {
            put_varint(&mut pos_table, zerocnt << 1);
            zerocnt = 0;
        }
        let pos = fpos;
        put_varint(&mut pos_table, ((pos - prevpos) << 1) | 1);
        prevpos = pos;
        put_varint(&mut vc, 0); // wrlen = 0: uncompressed records follow
        vc.extend_from_slice(rb);
        fpos += 1 + rb.len() as u64;
    }
    if zerocnt > 0 {
        put_varint(&mut pos_table, zerocnt << 1);
    }
    // Time table: absolute first time, then deltas.
    let mut tt: Vec<u8> = Vec::new();
    put_varint(&mut tt, times[0]);
    for w in times.windows(2) {
        put_varint(&mut tt, w[1] - w[0]);
    }
    let tt_z = zlib_store(&tt);
    vc.extend_from_slice(&pos_table);
    put_u64be(&mut vc, pos_table.len() as u64);
    vc.extend_from_slice(&tt_z);
    put_u64be(&mut vc, tt.len() as u64);
    put_u64be(&mut vc, tt_z.len() as u64);
    put_u64be(&mut vc, times.len() as u64);

    let mut vc_block = vec![1u8];
    put_u64be(&mut vc_block, (8 + vc.len()) as u64);
    vc_block.extend_from_slice(&vc);

    // ---- geometry block ----
    let mut geom_data: Vec<u8> = Vec::new();
    for h in 0..n {
        // geom encodes reals as a zero-width varint
        put_varint(&mut geom_data, if reals[h] { 0 } else { widths[h] as u64 });
    }
    let geom_z = zlib_store(&geom_data);
    let mut geom_block = vec![3u8];
    put_u64be(&mut geom_block, (24 + geom_z.len()) as u64);
    put_u64be(&mut geom_block, geom_data.len() as u64);
    put_u64be(&mut geom_block, n as u64);
    geom_block.extend_from_slice(&geom_z);

    // ---- hierarchy block (gzip) ----
    let mut hier: Vec<u8> = Vec::new();
    let mut hier_scopes = 0usize;
    fn emit_hier(
        id: usize,
        flat: &Flat,
        children: &Vec<Vec<usize>>,
        scope_path: &Vec<String>,
        order: &[usize],
        handle_of: &[usize],
        widths: &[usize],
        reals: &[bool],
        hier: &mut Vec<u8>,
        count: &mut usize,
    ) {
        hier.push(254); // FST_ST_VCD_SCOPE
        hier.push(0); // module
        put_cstr(hier, &flat.scopes[id].name);
        put_cstr(hier, &scope_path[id]);
        *count += 1;
        for &si in order {
            if flat.signals[si].scope as usize != id {
                continue;
            }
            let h = handle_of[si];
            let sig = &flat.signals[si];
            let path = &scope_path[id];
            let local = if path.is_empty() {
                sig.name.as_str()
            } else {
                sig.name
                    .strip_prefix(&format!("{}.", path))
                    .unwrap_or(&sig.name)
            };
            let (vt, vd, len) = match sig.kind {
                // FST_VT_VCD_REAL: 8 raw f64 bytes, geom width 0
                hdl_ir::SigKind::Real => (3u8, 0u8, 8u64),
                hdl_ir::SigKind::Reg => (5u8, 0u8, widths[h] as u64), // FST_VT_VCD_REG
                hdl_ir::SigKind::Input => (16, 1, widths[h] as u64),
                hdl_ir::SigKind::Output => (16, 2, widths[h] as u64),
                hdl_ir::SigKind::Inout => (16, 3, widths[h] as u64),
                _ => (16, 0, widths[h] as u64), // FST_VT_VCD_WIRE
            };
            hier.push(vt);
            hier.push(vd);
            put_cstr(hier, local);
            put_varint(hier, len);
            put_varint(hier, 0); // no alias: new handle (implicitly h+1 by order)
        }
        for &ch in &children[id] {
            emit_hier(
                ch, flat, children, scope_path, order, handle_of, widths, reals, hier, count,
            );
        }
        hier.push(255); // upscope
    }
    if !flat.scopes.is_empty() {
        emit_hier(
            flat.top_scope as usize,
            flat,
            &children,
            &scope_path,
            &order,
            &handle_of,
            &widths,
            &reals,
            &mut hier,
            &mut hier_scopes,
        );
    }
    let hier_z = gzip_store(&hier);
    let mut hier_block = vec![4u8];
    put_u64be(&mut hier_block, (16 + hier_z.len()) as u64);
    put_u64be(&mut hier_block, hier.len() as u64);
    hier_block.extend_from_slice(&hier_z);

    // ---- header block ----
    let mut hdr: Vec<u8> = Vec::with_capacity(330);
    hdr.push(0); // FST_BL_HDR
    put_u64be(&mut hdr, 329);
    put_u64be(&mut hdr, start);
    put_u64be(&mut hdr, end);
    hdr.extend_from_slice(&2.7182818284590452354f64.to_le_bytes()); // endian test
    put_u64be(&mut hdr, 0); // memory used by writer
    put_u64be(&mut hdr, hier_scopes as u64);
    put_u64be(&mut hdr, n as u64); // hier var entries
    put_u64be(&mut hdr, n as u64); // max handle
    put_u64be(&mut hdr, 1); // VC sections
    hdr.push(TIMESCALE_FS as u8);
    let mut ver = [0u8; 128];
    ver[..WRITER_STR.len()].copy_from_slice(WRITER_STR.as_bytes());
    hdr.extend_from_slice(&ver);
    let mut date = [0u8; 119];
    date[..6].copy_from_slice(b"hdlsim");
    hdr.extend_from_slice(&date);
    hdr.push(0); // filetype: VCD
    put_u64be(&mut hdr, 0); // timezero
    debug_assert_eq!(hdr.len(), 330);

    let mut out = hdr;
    out.extend_from_slice(&vc_block);
    out.extend_from_slice(&geom_block);
    out.extend_from_slice(&hier_block);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fst::read_fst;
    use hdl_ir::elab::{Flat, FlatSig, Scope};
    use hdl_ir::value::{V0, V1, VX};
    use hdl_ir::SigKind;

    fn flat() -> Flat {
        Flat {
            signals: vec![
                FlatSig {
                    name: "tb.clk".to_string(),
                    width: 1,
                    kind: SigKind::Wire,
                    init: Some(Value::from_bit(V0)),
                    mem: None,
                    scope: 0,
                    signed: false,
                    real_ranges: Vec::new(),
                },
                FlatSig {
                    name: "tb.dut.q".to_string(),
                    width: 8,
                    kind: SigKind::Reg,
                    init: Some(Value::x(8)),
                    mem: None,
                    scope: 1,
                    signed: false,
                    real_ranges: Vec::new(),
                },
                FlatSig {
                    name: "tb.dut.busy".to_string(),
                    width: 1,
                    kind: SigKind::Output,
                    init: Some(Value::from_bit(V1)),
                    mem: None,
                    scope: 1,
                    signed: false,
                    real_ranges: Vec::new(),
                },
            ],
            procs: Vec::new(),
            assigns: Vec::new(),
            functions: Vec::new(),
            scopes: vec![
                Scope {
                    name: "tb".to_string(),
                    parent: None,
                },
                Scope {
                    name: "dut".to_string(),
                    parent: Some(0),
                },
            ],
            top_scope: 0,
        }
    }

    fn events() -> Vec<SimEvent> {
        vec![
            SimEvent::TimeAdvance { time_fs: 0 },
            SimEvent::SignalChange {
                sig: 0,
                old: Value::from_bit(V0),
                new: Value::from_bit(V1),
                time_fs: 5,
            },
            SimEvent::TimeAdvance { time_fs: 5 },
            SimEvent::SignalChange {
                sig: 1,
                old: Value::x(8),
                new: Value::from_u64(8, 0xA5),
                time_fs: 15,
            },
            SimEvent::SignalChange {
                sig: 2,
                old: Value::from_bit(V1),
                new: Value::from_bit(V0),
                time_fs: 15,
            },
            SimEvent::TimeAdvance { time_fs: 15 },
            SimEvent::SignalChange {
                sig: 1,
                old: Value::from_u64(8, 0xA5),
                new: Value::x(8),
                time_fs: 25,
            },
            SimEvent::TimeAdvance { time_fs: 25 },
            SimEvent::Finish { time_fs: 25 },
        ]
    }

    #[test]
    fn fst_roundtrip() {
        let f = flat();
        let evs = events();
        let mut buf: Vec<u8> = Vec::new();
        let mut sink = FstSink::new(&mut buf, &f);
        for ev in &evs {
            sink.on_event(ev, &f);
        }
        sink.finish(&f);

        let (flat2, events2) = read_fst(&buf).unwrap();
        assert_eq!(flat2.signals.len(), 3);
        // Handles follow hierarchy order: tb.clk (scope tb), then dut scope vars.
        assert_eq!(flat2.signals[0].name, "tb.clk");
        assert_eq!(flat2.signals[1].name, "tb.dut.q");
        assert_eq!(flat2.signals[2].name, "tb.dut.busy");
        assert_eq!(flat2.signals[1].width, 8);
        assert_eq!(flat2.scopes.len(), 2);
        assert_eq!(flat2.scopes[1].name, "dut");
        assert_eq!(flat2.scopes[1].parent, Some(0));
        // Initial values from the bits array.
        assert_eq!(flat2.signals[0].init.as_ref().unwrap().bit(0), V0);
        assert_eq!(flat2.signals[1].init.as_ref().unwrap().bit(0), VX);
        assert_eq!(flat2.signals[2].init.as_ref().unwrap().bit(0), V1);

        // Events: time advances at 5/15/25 + changes with right values/times.
        let changes: Vec<&SimEvent> = events2
            .iter()
            .filter(|e| matches!(e, SimEvent::SignalChange { .. }))
            .collect();
        assert_eq!(changes.len(), 4);
        match changes[0] {
            SimEvent::SignalChange {
                sig, new, time_fs, ..
            } => {
                assert_eq!(*sig, 0);
                assert_eq!(new.bit(0), V1);
                assert_eq!(*time_fs, 5);
            }
            e => panic!("{:?}", e),
        }
        match changes[1] {
            SimEvent::SignalChange {
                sig, new, time_fs, ..
            } => {
                assert_eq!(*sig, 1);
                assert_eq!(new.bits, Value::from_u64(8, 0xA5).bits);
                assert_eq!(*time_fs, 15);
            }
            e => panic!("{:?}", e),
        }
        match changes[2] {
            SimEvent::SignalChange {
                sig, new, time_fs, ..
            } => {
                assert_eq!(*sig, 2);
                assert_eq!(new.bit(0), V0);
                assert_eq!(*time_fs, 15);
            }
            e => panic!("{:?}", e),
        }
        match changes[3] {
            SimEvent::SignalChange {
                sig, new, time_fs, ..
            } => {
                assert_eq!(*sig, 1);
                assert_eq!(new.bit(0), VX);
                assert_eq!(*time_fs, 25);
            }
            e => panic!("{:?}", e),
        }
        let advances: Vec<u64> = events2
            .iter()
            .filter_map(|e| match e {
                SimEvent::TimeAdvance { time_fs } => Some(*time_fs),
                _ => None,
            })
            .collect();
        assert_eq!(advances, vec![0, 5, 15, 25]);
    }

    #[test]
    fn fst_real_signal_roundtrip() {
        use hdl_ir::SigKind;
        let mut f = flat();
        f.signals.push(FlatSig {
            name: "tb.v".to_string(),
            width: 64,
            kind: SigKind::Real,
            init: Some(Value::from_f64(1.5)),
            mem: None,
            scope: 0,
            signed: false,
            real_ranges: Vec::new(),
        });
        let vsig = 3u32; // flat index of tb.v
        let mut buf: Vec<u8> = Vec::new();
        let mut sink = FstSink::new(&mut buf, &f);
        for ev in [
            SimEvent::TimeAdvance { time_fs: 0 },
            SimEvent::SignalChange {
                sig: vsig,
                old: Value::from_f64(1.5),
                new: Value::from_f64(2.75),
                time_fs: 10,
            },
            SimEvent::SignalChange {
                sig: vsig,
                old: Value::from_f64(2.75),
                new: Value::from_f64(-0.5),
                time_fs: 20,
            },
        ] {
            sink.on_event(&ev, &f);
        }
        sink.finish(&f);

        let (flat2, events2) = read_fst(&buf).unwrap();
        assert_eq!(flat2.signals.len(), 4);
        let h = flat2
            .signals
            .iter()
            .position(|s| s.name == "tb.v")
            .expect("tb.v present");
        let v = &flat2.signals[h];
        assert_eq!(v.kind, SigKind::Real);
        assert_eq!(v.init.as_ref().unwrap().to_f64(), Some(1.5));
        let reals: Vec<f64> = events2
            .iter()
            .filter_map(|e| match e {
                SimEvent::SignalChange { sig, new, .. } if *sig as usize == h => new.to_f64(),
                _ => None,
            })
            .collect();
        assert_eq!(reals, vec![2.75, -0.5]);
    }

    #[test]
    fn fst_empty_trace_is_valid() {
        let f = flat();
        let mut buf: Vec<u8> = Vec::new();
        let sink = FstSink::new(&mut buf, &f);
        drop(sink);
        let mut sink = FstSink::new(&mut buf, &f);
        sink.finish(&f);
        let (flat2, events2) = read_fst(&buf).unwrap();
        assert_eq!(flat2.signals.len(), 3);
        // No changes; only the time-0 advance from the (single-entry) time table.
        assert!(events2
            .iter()
            .all(|e| matches!(e, SimEvent::TimeAdvance { time_fs: 0 })));
    }
}
