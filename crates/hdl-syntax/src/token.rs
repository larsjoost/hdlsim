//! Tokens produced by the lexers.
//!
//! One shared token vocabulary for both languages; the text of a token is always
//! the exact source slice, so lossless reconstruction is possible.

use crate::source::Span;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TokenKind {
    /// Identifier: `[A-Za-z_][A-Za-z0-9_$]*`.
    Ident,
    /// Integer literal, including based forms (`16'hDEAD`, `'b101`); the base
    /// and digits stay in the token text and are interpreted by the parser.
    IntLiteral,
    /// Real literal (`3.14`, `1.0e-3`).
    RealLiteral,
    /// String literal, quotes included in the text.
    StrLiteral,
    /// One punctuation character (text holds the character).
    Punct,
    /// Whitespace run (kept in the token stream for the lossless CST).
    Whitespace,
    /// Comment (kept, like whitespace).
    Comment,
    /// End of file (always the last token).
    Eof,
}

#[derive(Clone, Debug)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

impl Token {
    pub fn new(kind: TokenKind, span: Span) -> Token {
        Token { kind, span }
    }
}
