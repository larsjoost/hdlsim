//! # hdl-lib
//!
//! Library manager: named logical libraries mapped to directories, automatic
//! dependency search, on-demand compilation, dependency graph, incremental cache.
//!
//! Pipeline position: between `hdl-syntax` (parsing) and `hdl-ir` (elaboration).
//!
//! Extension seam: [`providers`] — a library is anything implementing
//! `trait UnitProvider` (filesystem dir, built-in `std`/`ieee`, cache; later zip
//! or remote providers). Resolution code never changes when a provider is added.
//!
//! Invariants:
//! - Resolution order is deterministic: work → configured `search_order` libs →
//!   Verilog libdirs/libfiles. Every miss reports everything that was searched.
//! - The dependency graph is acyclic; cycles are reported with the cycle path.

pub mod providers;
