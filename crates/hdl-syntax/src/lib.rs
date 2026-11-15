//! # hdl-syntax
//!
//! Reusable lexing, parsing, and syntax trees for Verilog and VHDL.
//!
//! Pipeline position: first stage. Everything downstream (library manager,
//! elaboration, simulator, wave display) consumes the trees produced here. This
//! crate is deliberately independent of the simulator so other applications
//! (linters, formatters, LSP servers, doc generators) can depend on it alone.
//!
//! # Layout
//! - [`source`]: multi-file source database and byte spans.
//! - [`diagnostic`]: error/warning values with stable codes; never panics.
//! - [`token`]: token kinds shared by the lexers.
//! - [`cst`]: lossless concrete syntax tree (hand-rolled rowan-style green/red).
//! - [`language`]: the `Language` enum and the `LanguageFrontend` registry seam.
//! - [`verilog`], [`vhdl`]: per-language lexer, parser, and typed AST.
//!
//! # Invariants
//! - Lexing and parsing never panic on arbitrary input; failures are diagnostics.
//! - Every tree node carries a byte range into a [`source::SourceFile`].
//! - Output is deterministic: equal inputs produce equal trees and diagnostics.

pub mod cst;
pub mod diagnostic;
pub mod fmt;
pub mod kinds;
pub mod language;
pub mod source;
pub mod token;
pub mod verilog;
pub mod vhdl;
