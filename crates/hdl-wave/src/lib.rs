//! # hdl-wave
//!
//! Wave sinks and viewers — the graphical display of a simulation.
//!
//! - VCD sink (`--vcd out.vcd`) for interchange.
//! - Self-contained interactive HTML/SVG viewer (`--wave out.html`): one file,
//!   embedded CSS/JS, works offline in any browser; digital traces, buses as
//!   hex, cursor + value readout, pan/zoom, search, transcript.
//! - Live wave window (`--live [--port N]`): built-in `std::net` HTTP server +
//!   Server-Sent Events streaming value changes while the sim runs, with a
//!   Stop control sent back over HTTP.
//!
//! Extension seam: `trait WaveSink` — a new wave format is a new module plus one
//! registration line in [`sinks`]. Sinks receive the same `SimEvent` stream as
//! every other `hdl-sim` observer, and flush buffered output in `finish`.
//!
//! Invariant: sinks handle arbitrary signal widths; the HTML pages never need
//! the network (all assets embedded).

pub mod fst;
pub mod sinks;

pub use sinks::html::HtmlSink;
pub use sinks::live::{spawn_live_server, LiveBus, LiveSink};
pub use sinks::vcd::VcdSink;
pub use sinks::{SinkAdapter, WaveSink};
