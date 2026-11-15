//! Wave sink registry — the extension seam for new wave formats.
//!
//! Implement [`WaveSink`] and register it here. Built-ins: `vcd`, `html`,
//! `ascii`, `live` (HTTP+SSE), `hwf`. Each sink receives the same [`SimEvent`]
//! stream as every other `hdl-sim` observer, plus the elaborated [`Flat`] for
//! signal metadata.

use hdl_ir::elab::Flat;
use hdl_sim::events::SimEvent;

/// Extension seam (see `docs/extensibility.md`): a new wave format is a new
/// module implementing this trait plus one entry in [`sinks`].
pub trait WaveSink {
    fn on_event(&mut self, ev: &SimEvent, flat: &Flat);
    /// Called once when the simulation ends — sinks that buffer (html, ascii)
    /// flush their output file here.
    fn finish(&mut self, _flat: &Flat) {}
}

/// A sink that adapts a `WaveSink` to the plain `Observer` interface.
pub struct SinkAdapter<S: WaveSink> {
    pub sink: S,
    pub flat: Flat,
}

impl<S: WaveSink> hdl_sim::Observer for SinkAdapter<S> {
    fn on_event(&mut self, ev: &SimEvent) {
        self.sink.on_event(ev, &self.flat);
    }
    fn on_finish(&mut self) {
        let flat = self.flat.clone();
        self.sink.finish(&flat);
    }
}

pub mod fst;
pub mod html;
pub mod hwf;
pub mod live;
pub mod vcd;

// ---- shared helpers -------------------------------------------------------------

use hdl_ir::value::{Value, V0, V1, VZ};

/// Bits as an MSB-first string of 0/1/x/z for display/JSON.
pub(crate) fn bits_msb(v: &Value) -> String {
    v.bits
        .iter()
        .rev()
        .map(|&b| match b {
            V0 => '0',
            V1 => '1',
            VZ => 'z',
            _ => 'x',
        })
        .collect()
}

/// Minimal JSON string escaping (also neutralizes `</script>` breakouts).
pub(crate) fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '<' => out.push_str("\\u003c"),
            '>' => out.push_str("\\u003e"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
