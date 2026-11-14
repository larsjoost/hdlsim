//! # hdl-sim
//!
//! Event-driven simulation kernel: multi-state logic values, time, delta-cycle
//! scheduler, and an observer bus. Embeddable as a library — other applications
//! can run a simulation and react to events with their own observers.
//!
//! Pipeline position: consumes `hdl-ir`, emits `SimEvent`s to observers
//! (wave sinks, coverage, debugger hooks).
//!
//! Semantics:
//! - Delta cycles run until stable, then time advances to the next event.
//! - Verilog: 4-state values (0/1/X/Z), blocking + non-blocking assignment regions,
//!   `#delay`.
//! - VHDL: 9-state `std_logic` with resolution, signals update at end of delta.
//!
//! Extension seams: `trait Observer` (closed `SimEvent` enum — new analyses are
//! new observers, zero kernel edits) and a name→fn registry for system tasks.
//!
//! Invariants:
//! - No allocation in the event loop.
//! - Observers receive immutable event snapshots and cannot corrupt the kernel.
//! - Simulation is deterministic for a fixed seed.
