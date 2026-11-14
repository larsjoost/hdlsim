//! VHDL front-end: lexer, recursive-descent parser (one function per IEEE 1076
//! grammar production), and typed AST views over the CST.
//!
//! Covers the practical subset first (entities, architectures, packages,
//! processes, concurrent assignments); VHDL-2008 constructs are added behind
//! dialect flags.

pub mod lexer;
pub mod parser;
