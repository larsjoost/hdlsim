//! Verilog front-end: lexer, recursive-descent parser (one function per IEEE 1364
//! grammar production), and typed AST views over the CST.
//!
//! Dialect differences from SystemVerilog are driven by a `DialectFlags` config,
//! not by forking this module.

pub mod lexer;
