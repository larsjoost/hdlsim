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
//! every match), dumpable to text so other tools (synthesis, formal, coverage)
//! can consume a whole elaborated design without parsing HDL.
//!
//! # Layout
//! - [`value`]: multi-state logic values (9-state capable, Verilog uses 4).
//! - [`ir`]: the module-level IR (pre-elaboration).
//! - [`elab`]: elaboration — flattens instance hierarchy into one signal space.
//!
//! # Invariants
//! - Lowering is deterministic and uses only the passed `ResolveCtx` for names.
//! - All ids (`SigId`, `ProcId`, …) are arena indices, never pointers.
//! - After elaboration, every `Expr::Sig` refers to the flat signal space.

pub mod elab;
pub mod ir;
pub mod value;

pub use ir::*;
pub use value::{BinOp, UnOp, Value};
