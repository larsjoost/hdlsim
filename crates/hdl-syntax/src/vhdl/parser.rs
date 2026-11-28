//! VHDL parser: tokens → lossless CST (rowan-style via [`TreeBuilder`]).
//!
//! One function per grammar production, mirroring IEEE 1076 clause names.
//! This file is the shell: the [`ParsedFile`] result, [`parse_file`], and
//! the shared [`Parser`] state (token stream, builder, diagnostics, and the
//! waveform-position counter). The grammar lives in child modules, one per
//! clause family:
//!
//! - [`lex`](self::lex): token navigation/lookahead, consumption, error recovery
//! - [`design`](self::design): design file, library/use/context, entities,
//!   architectures, packages, configurations
//! - [`decl`](self::decl): declarative-item dispatch and declaration forms
//! - [`iface`](self::iface): generic/port/parameter interface lists
//! - [`types`](self::types): type declarations, subtypes, constraints
//! - [`assoc`](self::assoc): association lists and formal lookahead
//! - [`conc`](self::conc): concurrent statements, blocks, generates
//! - [`seq`](self::seq): sequential statements
//! - [`name`](self::name): names, external names, suffix chains
//! - [`expr`](self::expr): expression chain and waveform elements
//! - [`tests`](self::tests) / [`tests_stmt`](self::tests_stmt): tests
//!
//! Covers VHDL-93/2002 plus the VHDL-2008 constructs in common use, with
//! VHDL-2019 additions where noted (view declarations, type generics,
//! conditional `??`, sized bit-strings with sign markers). VHDL is
//! case-insensitive: keyword checks lowercase the token text; the CST keeps
//! the original text (lossless).
//!
//! Recovery: unknown constructs produce an `ErrorToken` + diagnostic and the
//! parser skips to the next `;` or a known keyword, so one bad unit does not
//! kill the rest of the file.

use crate::cst::{SyntaxNode, TreeBuilder};
use crate::diagnostic::Diagnostic;
use crate::source::{FileId, SourceFile};
use crate::token::Token;

/// Output of parsing one VHDL file.
pub struct ParsedFile {
    pub root: SyntaxNode,
    pub diags: Vec<Diagnostic>,
}

/// Lex + parse one file.
pub fn parse_file(file: FileId, src: &SourceFile) -> ParsedFile {
    let (tokens, lex_diags) = super::lexer::lex(file, src);
    let mut p = Parser {
        src,
        tokens: &tokens,
        pos: 0,
        builder: TreeBuilder::new(file),
        diags: lex_diags,
        in_waveform: 0,
    };
    p.parse_design_file();
    let root = p.builder.finish();
    ParsedFile {
        root,
        diags: p.diags,
    }
}

mod assoc;
mod conc;
mod decl;
mod design;
mod expr;
mod iface;
mod lex;
mod name;
mod seq;
mod types;

struct Parser<'a> {
    src: &'a SourceFile,
    tokens: &'a [Token],
    pos: usize,
    builder: TreeBuilder,
    diags: Vec<Diagnostic>,
    /// >0 while parsing a waveform value, where `when`/`else` belongs to the
    /// > waveform (conditional signal assignment), not to a conditional
    /// > expression.
    in_waveform: u32,
}

impl<'a> Parser<'a> {}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod tests_stmt;
