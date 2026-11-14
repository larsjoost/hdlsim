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
}

pub mod vcd;
