//! The syntax-kind vocabulary: one closed enum covering every token and node kind
//! of every language. Token kinds mirror [`crate::token::TokenKind`] 1:1 via
//! [`SyntaxKind::from_token`]; node kinds are added per language as its parser
//! grows (verilog::*, vhdl::*).

use crate::token::TokenKind;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum SyntaxKind {
    // --- tokens (mirror TokenKind) ---
    Ident,
    IntLiteral,
    RealLiteral,
    StrLiteral,
    /// `$name` system task/function.
    SysTask,
    /// `` `name `` compiler directive / macro reference.
    Directive,
    /// One punctuation character.
    Punct,
    Whitespace,
    Comment,
    /// A character or sequence the lexer could not classify.
    Error,
    Eof,

    // --- generic nodes ---
    /// Root of a file's tree. Invariant: a tree has exactly one Root.
    Root,
    /// A token the parser could not place into the grammar (recovery node).
    ErrorToken,
}

impl SyntaxKind {
    pub fn from_token(kind: TokenKind) -> SyntaxKind {
        match kind {
            TokenKind::Ident => SyntaxKind::Ident,
            TokenKind::IntLiteral => SyntaxKind::IntLiteral,
            TokenKind::RealLiteral => SyntaxKind::RealLiteral,
            TokenKind::StrLiteral => SyntaxKind::StrLiteral,
            TokenKind::SysTask => SyntaxKind::SysTask,
            TokenKind::Directive => SyntaxKind::Directive,
            TokenKind::Punct => SyntaxKind::Punct,
            TokenKind::Whitespace => SyntaxKind::Whitespace,
            TokenKind::Comment => SyntaxKind::Comment,
            TokenKind::Error => SyntaxKind::Error,
            TokenKind::Eof => SyntaxKind::Eof,
        }
    }

    pub fn is_token(self) -> bool {
        matches!(
            self,
            SyntaxKind::Ident
                | SyntaxKind::IntLiteral
                | SyntaxKind::RealLiteral
                | SyntaxKind::StrLiteral
                | SyntaxKind::SysTask
                | SyntaxKind::Directive
                | SyntaxKind::Punct
                | SyntaxKind::Whitespace
                | SyntaxKind::Comment
                | SyntaxKind::Error
                | SyntaxKind::Eof
        )
    }
}
