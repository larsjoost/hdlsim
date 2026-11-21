//! # hdlsim
//!
//! The command-line interface crate, as a library so integration tests (and
//! other tools) can drive the same pipeline: `compile` (sources → `Design`)
//! plus the `commands` registry. The crate is intentionally thin: argument
//! parsing and I/O only — all logic lives in the library crates (`hdl-syntax`,
//! `hdl-lib`, `hdl-ir`, `hdl-sim`, `hdl-wave`).
//!
//! Extension seam: `trait Command` registered in `commands` — a new subcommand
//! is a new module plus one registration line. See `docs/extensibility.md`.

pub mod commands;
pub mod compile;
pub mod json;
pub mod phase;
pub mod project;
pub mod tcl;
