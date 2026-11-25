//! `.hwf` — hdlsim's own compact binary wave format (`--hwf out.hwf`), plus a
//! reader that replays it into any other sink (e.g. the HTML viewer via
//! `hdlsim wave file.hwf -o out.html`): simulate once, view offline many times.
//!
//! Layout (little-endian): magic `HDLHWF02`, u32 version, then the signal table
//! (u32 count; per signal: str name, u32 width, u8 flags [bit0=mem, bit1=real],
//! value init bits), then a tagged event stream: 0=end, 1=signal change (u64 t,
//! u32 sig, value bits), 2=message (u64 t, str), 3=time advance (u64 t).
//! v1 files (`HDLHWF01`, one u8 is_mem per signal) are still readable.

use super::WaveSink;
use hdl_ir::elab::{Flat, FlatSig};
use hdl_ir::value::Value;
use hdl_ir::SigKind;
use hdl_sim::events::SimEvent;
use std::io::Write;

const MAGIC: &[u8; 8] = b"HDLHWF02";
const MAGIC_V1: &[u8; 8] = b"HDLHWF01";
const VERSION: u32 = 2;
const FLAG_MEM: u8 = 1;
const FLAG_REAL: u8 = 2;

fn w_u8(w: &mut impl Write, v: u8) {
    let _ = w.write_all(&[v]);
}
fn w_u32(w: &mut impl Write, v: u32) {
    let _ = w.write_all(&v.to_le_bytes());
}
fn w_u64(w: &mut impl Write, v: u64) {
    let _ = w.write_all(&v.to_le_bytes());
}
fn w_str(w: &mut impl Write, s: &str) {
    w_u32(w, s.len() as u32);
    let _ = w.write_all(s.as_bytes());
}
fn w_value(w: &mut impl Write, v: &Value) {
    w_u32(w, v.bits.len() as u32);
    let _ = w.write_all(&v.bits);
}

/// Streaming `.hwf` writer. The header (signal table + initial values) is
/// written at construction; events are appended as they arrive.
pub struct HwfSink<W: Write> {
    w: W,
}

impl<W: Write> HwfSink<W> {
    pub fn new(mut w: W, flat: &Flat) -> HwfSink<W> {
        let _ = w.write_all(MAGIC);
        w_u32(&mut w, VERSION);
        w_u32(&mut w, flat.signals.len() as u32);
        for s in &flat.signals {
            w_str(&mut w, &s.name);
            w_u32(&mut w, s.width.max(1) as u32);
            let mut flags = 0u8;
            if s.mem.is_some() {
                flags |= FLAG_MEM;
            }
            if s.kind == SigKind::Real {
                flags |= FLAG_REAL;
            }
            w_u8(&mut w, flags);
            w_value(&mut w, s.init.as_ref().unwrap_or(&Value::x(s.width.max(1))));
        }
        HwfSink { w }
    }
}

impl<W: Write> WaveSink for HwfSink<W> {
    fn on_event(&mut self, ev: &SimEvent, _flat: &Flat) {
        match ev {
            SimEvent::SignalChange {
                sig, new, time_fs, ..
            } => {
                w_u8(&mut self.w, 1);
                w_u64(&mut self.w, *time_fs);
                w_u32(&mut self.w, *sig);
                w_value(&mut self.w, new);
            }
            SimEvent::Message { text, time_fs, .. } => {
                w_u8(&mut self.w, 2);
                w_u64(&mut self.w, *time_fs);
                w_str(&mut self.w, text);
            }
            SimEvent::TimeAdvance { time_fs } => {
                w_u8(&mut self.w, 3);
                w_u64(&mut self.w, *time_fs);
            }
            _ => {}
        }
    }
    fn finish(&mut self, _flat: &Flat) {
        w_u8(&mut self.w, 0);
        let _ = self.w.flush();
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
            .ok_or("truncated .hwf")?;
        self.pos += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
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
    fn str(&mut self) -> Result<String, String> {
        let n = self.u32()? as usize;
        if n > (1 << 24) {
            return Err("string too long".to_string());
        }
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

/// Read a `.hwf` file back into a minimal `Flat` (signals only) plus the event
/// stream, ready to replay into any `WaveSink`.
pub fn read_hwf(bytes: &[u8]) -> Result<(Flat, Vec<SimEvent>), String> {
    let v1 = bytes.get(..8) == Some(MAGIC_V1);
    if !v1 && bytes.get(..8) != Some(MAGIC) {
        return Err("not an hdlsim .hwf file (bad magic)".to_string());
    }
    let mut r = R { buf: bytes, pos: 8 };
    let version = r.u32()?;
    if version != VERSION && !(v1 && version == 1) {
        return Err(format!("unsupported .hwf version {}", version));
    }
    let n_sig = r.u32()? as usize;
    if n_sig > (1 << 24) {
        return Err("too many signals".to_string());
    }
    let mut signals = Vec::with_capacity(n_sig);
    for _ in 0..n_sig {
        let name = r.str()?;
        let width = r.u32()? as usize;
        let flags = if v1 { r.u8()? & FLAG_MEM } else { r.u8()? };
        let init = r.value()?;
        signals.push(FlatSig {
            name,
            width,
            kind: if flags & FLAG_REAL != 0 {
                SigKind::Real
            } else {
                SigKind::Wire
            },
            init: Some(init),
            mem: (flags & FLAG_MEM != 0).then_some(0),
            scope: 0,
            signed: false,
            real_ranges: Vec::new(),
        });
    }
    let mut events = Vec::new();
    loop {
        match r.u8()? {
            0 => break,
            1 => {
                let t = r.u64()?;
                let sig = r.u32()?;
                let new = r.value()?;
                let old = Value::x(new.bits.len());
                events.push(SimEvent::SignalChange {
                    sig,
                    old,
                    new,
                    time_fs: t,
                });
            }
            2 => {
                let t = r.u64()?;
                let text = r.str()?;
                events.push(SimEvent::Message {
                    text,
                    time_fs: t,
                    span: None,
                });
            }
            3 => {
                let t = r.u64()?;
                events.push(SimEvent::TimeAdvance { time_fs: t });
            }
            t => return Err(format!("bad event tag {}", t)),
        }
    }
    if r.pos != bytes.len() {
        return Err("trailing bytes in .hwf".to_string());
    }
    let flat = Flat {
        signals,
        procs: Vec::new(),
        assigns: Vec::new(),
        functions: Vec::new(),
        scopes: Vec::new(),
        top_scope: 0,
    };
    Ok((flat, events))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hdl_ir::elab::Flat;
    use hdl_ir::value::{V0, V1, VX};

    fn flat() -> Flat {
        Flat {
            signals: vec![
                FlatSig {
                    name: "clk".to_string(),
                    width: 1,
                    kind: SigKind::Wire,
                    init: Some(Value::from_bit(V0)),
                    mem: None,
                    scope: 0,
                    signed: false,
                    real_ranges: Vec::new(),
                },
                FlatSig {
                    name: "q".to_string(),
                    width: 8,
                    kind: SigKind::Wire,
                    init: Some(Value::x(8)),
                    mem: None,
                    scope: 0,
                    signed: false,
                    real_ranges: Vec::new(),
                },
            ],
            procs: Vec::new(),
            assigns: Vec::new(),
            functions: Vec::new(),
            scopes: Vec::new(),
            top_scope: 0,
        }
    }

    #[test]
    fn hwf_roundtrip() {
        let f = flat();
        let mut buf: Vec<u8> = Vec::new();
        let mut sink = HwfSink::new(&mut buf, &f);
        sink.on_event(&SimEvent::TimeAdvance { time_fs: 0 }, &f);
        sink.on_event(
            &SimEvent::SignalChange {
                sig: 0,
                old: Value::from_bit(V0),
                new: Value::from_bit(V1),
                time_fs: 5,
            },
            &f,
        );
        sink.on_event(
            &SimEvent::SignalChange {
                sig: 1,
                old: Value::x(8),
                new: Value::from_u64(8, 0xA5),
                time_fs: 15,
            },
            &f,
        );
        sink.on_event(
            &SimEvent::Message {
                text: "hello\n".to_string(),
                time_fs: 15,
                span: None,
            },
            &f,
        );
        sink.finish(&f);

        let (flat2, events) = read_hwf(&buf).unwrap();
        assert_eq!(flat2.signals.len(), 2);
        assert_eq!(flat2.signals[0].name, "clk");
        assert_eq!(flat2.signals[1].width, 8);
        assert_eq!(events.len(), 4);
        assert!(matches!(events[0], SimEvent::TimeAdvance { time_fs: 0 }));
        match &events[1] {
            SimEvent::SignalChange {
                sig, new, time_fs, ..
            } => {
                assert_eq!(*sig, 0);
                assert_eq!(new.bit(0), V1);
                assert_eq!(*time_fs, 5);
            }
            e => panic!("{:?}", e),
        }
        match &events[2] {
            SimEvent::SignalChange { sig, new, .. } => {
                assert_eq!(*sig, 1);
                assert_eq!(new.bits, Value::from_u64(8, 0xA5).bits);
            }
            e => panic!("{:?}", e),
        }
        assert!(
            matches!(&events[3], SimEvent::Message { text, time_fs, .. } if text == "hello\n" && *time_fs == 15)
        );
    }

    #[test]
    fn read_rejects_garbage() {
        assert!(read_hwf(b"NOPE....").is_err());
        let f = flat();
        let mut buf: Vec<u8> = Vec::new();
        let sink = HwfSink::new(&mut buf, &f);
        drop(sink);
        buf.push(9); // trailing byte -> error (no end tag written either)
        assert!(read_hwf(&buf).is_err());
        let _ = VX;
    }
}
