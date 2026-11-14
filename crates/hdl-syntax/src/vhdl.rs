//! VHDL front-end: lexer, recursive-descent parser (one function per IEEE 1076
//! grammar production), and typed AST views over the CST.
//!
//! Covers VHDL-93/2002 and the VHDL-2008 constructs in common use (see
//! `parser.rs`); remaining 2008 corners are added behind dialect flags.

pub mod lexer;
pub mod parser;
