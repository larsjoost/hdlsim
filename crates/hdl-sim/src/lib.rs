//! # hdl-sim
//!
//! Event-driven simulation kernel: multi-state values, time, delta-cycle
//! scheduler, observer bus. Embeddable as a library — other applications can run
//! a simulation and react to [`SimEvent`]s with their own [`Observer`]s.
//!
//! Pipeline position: consumes [`hdl_ir::elab::Flat`], emits events to observers
//! (wave sinks, coverage, live server).
//!
//! # Layout
//! - [`events`]: the closed `SimEvent` enum + `Observer` trait (extension seam).
//! - [`sim`]: the `Simulator` (active/NBA/time regions, process frames).
//! - [`rand`]: seeded RNG + constraint solver behind `$urandom`/`$rand_solve`.
//! - [`debug`]: debugging helpers — value/time formatting, a signal [`debug::Watcher`],
//!   and design introspection on the simulator.
//!
//! # Invariants
//! - Deterministic for a fixed design and RNG seed.
//! - Observers get immutable event snapshots; they cannot corrupt the kernel.
//! - The kernel never panics on design content; runtime problems are events.

pub mod bmc;
pub mod coverage;
pub mod debug;
pub mod events;
pub mod meminfo;
pub mod rand;
pub mod sat;
pub mod sim;

pub use events::{NullObserver, Observer, SimEvent};
pub use sim::{set_progress_interval, Simulator};
