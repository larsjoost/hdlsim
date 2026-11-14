//! # hdl-wave
//!
//! Wave sinks and viewers — the graphical display of a simulation.
//!
//! - VCD sink (`--vcd out.vcd`) for interchange.
//! - Self-contained interactive HTML/SVG viewer (`--wave out.html`): one file,
//!   embedded CSS/JS, works offline in any browser; signal tree by instance
//!   hierarchy, digital traces, buses as hex/dec, cursor + value readout,
//!   pan/zoom, search, markers, groups.
//! - Live wave window (`--live [--port N]`): built-in `std::net` HTTP server +
//!   Server-Sent Events streaming value changes in real time, with pause/step
//!   controls sent back over HTTP.
//! - ASCII snapshot (`--waves-ascii`) for logs/CI.
//! - `.hwf` compact binary format for huge sims.
//!
//! Extension seam: `trait WaveSink` — a new wave format is a new module plus one
//! registration line. Sinks receive the same `SimEvent` stream as every other
//! `hdl-sim` observer.
//!
//! Invariant: sinks handle arbitrary signal widths; the HTML viewer never needs
//! the network (all assets embedded).

pub mod sinks;

pub use sinks::vcd::VcdSink;
pub use sinks::{SinkAdapter, WaveSink};
