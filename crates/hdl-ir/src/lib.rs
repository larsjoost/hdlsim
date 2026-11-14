//! # hdl-ir
//!
//! Language-neutral elaborated design IR: modules, instances, signals, processes,
//! expressions, parameters. Both the Verilog and the VHDL front-end lower onto
//! this IR, which is what makes mixed-language simulation native.
//!
//! Pipeline position: after `hdl-lib` resolution, before `hdl-sim`.
//!
//! Extension seam: `trait Lowering` — a new front-end language implements it to
//! produce IR. The IR types themselves are a closed core (the compiler checks
//! every match), dumpable to text/binary so other tools (synthesis, formal,
//! coverage) can consume a whole elaborated design without parsing HDL.
//!
//! Invariants:
//! - Lowering is deterministic and uses only the passed `ResolveCtx` for names.
//! - All indices (signals, processes, instances) are arena indices, never pointers.
