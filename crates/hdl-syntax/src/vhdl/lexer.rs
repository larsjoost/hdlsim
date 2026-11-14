//! VHDL lexer: source text → tokens + diagnostics.
//!
//! Byte-oriented and panic-free, like the Verilog lexer. VHDL is
//! case-insensitive: identifiers/keywords keep their source case and the
//! *parser* compares lowercased text, so the CST stays lossless. Multi-char
//! operators (`<=`, `:=`, `=>`, `**`, `/=`, ...) are emitted as single `Punct`
//! tokens; the parser combines them.
//!
//! Literal corner cases handled here:
//! - `'0'` is a CharLiteral; `sig'event` is Ident + Punct('\'') (attribute tick).
//! - `x"FF"` / `b"01"` / `o"7"` are StrLiteral tokens with the base letter kept.
//! - `16#FF#` (based literal) and `1_000`, `3.14`, `1e-3` are Int/RealLiteral.

use crate::diagnostic::Diagnostic;
use crate::source::{FileId, SourceFile, Span};
use crate::token::{Token, TokenKind};

/// Lex a whole file. Returns tokens (whitespace/comments included, plus a
/// trailing zero-length `Eof`) and diagnostics, both in source order.
pub fn lex(file: FileId, src: &SourceFile) -> (Vec<Token>, Vec<Diagnostic>) {
    let mut lx = Lexer {
        text: src.text().as_bytes(),
        pos: 0,
        file,
        tokens: Vec::new(),
        diags: Vec::new(),
    };
    lx.run();
    let eof = Span::new(file, lx.pos as u32, lx.pos as u32);
    lx.tokens.push(Token::new(TokenKind::Eof, eof));
    (lx.tokens, lx.diags)
}

struct Lexer<'a> {
    text: &'a [u8],
    pos: usize,
    file: FileId,
    tokens: Vec<Token>,
    diags: Vec<Diagnostic>,
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

impl<'a> Lexer<'a> {
    fn peek(&self, n: usize) -> Option<u8> {
        self.text.get(self.pos + n).copied()
    }

    /// Emit a token of `len` bytes starting at the current position.
    fn emit(&mut self, kind: TokenKind, len: usize) {
        let start = self.pos;
        self.pos += len;
        self.emit_from(kind, start);
    }

    /// Emit a token covering `start..self.pos` (pos already advanced).
    fn emit_from(&mut self, kind: TokenKind, start: usize) {
        self.tokens.push(Token::new(
            kind,
            Span::new(self.file, start as u32, self.pos as u32),
        ));
    }

    fn err(&mut self, len: usize, msg: &str) {
        let start = self.pos;
        self.pos += len;
        let span = Span::new(self.file, start as u32, self.pos as u32);
        self.diags.push(Diagnostic::error("E0101", Some(span), msg));
        self.tokens.push(Token::new(TokenKind::Error, span));
    }

    fn run(&mut self) {
        while self.pos < self.text.len() {
            let b = self.text[self.pos];
            match b {
                b' ' | b'\t' | b'\r' | b'\n' | 0x0b | 0x0c => self
                    .run_of(TokenKind::Whitespace, |b| {
                        matches!(b, b' ' | b'\t' | b'\r' | b'\n' | 0x0b | 0x0c)
                    }),
                b'-' if self.peek(1) == Some(b'-') => self.line_comment(),
                b'"' => self.string(),
                b'\'' => self.tick_or_char(),
                b'\\' => self.extended_ident(),
                b'0'..=b'9' => self.number(),
                b if is_ident_start(b) => {
                    // Bit string literal: x"FF" / b"01" / o"17" (letter directly
                    // followed by a quote, not preceded by an identifier char).
                    if matches!(b, b'x' | b'X' | b'b' | b'B' | b'o' | b'O')
                        && self.peek(1) == Some(b'"')
                        && !self
                            .text
                            .get(self.pos.wrapping_sub(1))
                            .map_or(false, |&p| is_ident_continue(p))
                    {
                        self.emit(TokenKind::Ident, 1);
                        self.string();
                    } else {
                        self.ident()
                    }
                }
                _ if b.is_ascii() => self.emit(TokenKind::Punct, 1),
                _ => self.err(1, "unexpected byte in VHDL source"),
            }
        }
    }

    fn run_of(&mut self, kind: TokenKind, pred: impl Fn(u8) -> bool) {
        let start = self.pos;
        while self.pos < self.text.len() && pred(self.text[self.pos]) {
            self.pos += 1;
        }
        self.emit_from(kind, start);
    }

    fn line_comment(&mut self) {
        self.run_of(TokenKind::Comment, |b| b != b'\n');
    }

    fn string(&mut self) {
        // `"...""..."` — doubled quote is an escaped quote.
        let start = self.pos;
        self.pos += 1; // opening quote
        while self.pos < self.text.len() {
            if self.text[self.pos] == b'"' {
                if self.peek(1) == Some(b'"') {
                    self.pos += 2;
                } else {
                    self.pos += 1;
                    break;
                }
            } else {
                self.pos += 1;
            }
        }
        self.emit_from(TokenKind::StrLiteral, start);
    }

    /// `'0'` → CharLiteral; `''''` (apostrophe literal) → CharLiteral of len 4;
    /// otherwise the tick of an attribute/qualified expression → Punct('\'').
    fn tick_or_char(&mut self) {
        if self.peek(1) == Some(b'\'') && self.peek(2) == Some(b'\'') {
            self.emit(TokenKind::CharLiteral, 4); // ''''
        } else if self.peek(2) == Some(b'\'') && self.peek(1).is_some() {
            self.emit(TokenKind::CharLiteral, 3); // 'x'
        } else {
            self.emit(TokenKind::Punct, 1);
        }
    }

    /// `\extended identifier\`.
    fn extended_ident(&mut self) {
        let start = self.pos;
        self.pos += 1;
        while self.pos < self.text.len() && self.text[self.pos] != b'\\' {
            self.pos += 1;
        }
        if self.pos < self.text.len() {
            self.pos += 1; // closing backslash
        }
        self.emit_from(TokenKind::Ident, start);
    }

    fn ident(&mut self) {
        self.run_of(TokenKind::Ident, is_ident_continue);
    }

    /// Decimal integer/real, or based literal `base#digits#[e±n]`.
    fn number(&mut self) {
        let start = self.pos;
        while self.pos < self.text.len()
            && (self.text[self.pos].is_ascii_digit() || self.text[self.pos] == b'_')
        {
            self.pos += 1;
        }
        if self.peek(0) == Some(b'#') {
            // Based literal: consume up to and including the closing '#'.
            self.pos += 1;
            while self.pos < self.text.len() && self.text[self.pos] != b'#' {
                self.pos += 1;
            }
            if self.pos < self.text.len() {
                self.pos += 1; // closing '#'
            }
            self.exponent();
            self.emit_from(TokenKind::IntLiteral, start);
            return;
        }
        let mut real = false;
        if self.peek(0) == Some(b'.') && self.peek(1).map_or(false, |c| c.is_ascii_digit()) {
            real = true;
            self.pos += 1;
            while self.pos < self.text.len()
                && (self.text[self.pos].is_ascii_digit() || self.text[self.pos] == b'_')
            {
                self.pos += 1;
            }
        }
        if real || self.exponent() {
            self.emit_from(TokenKind::RealLiteral, start);
        } else {
            self.emit_from(TokenKind::IntLiteral, start);
        }
    }

    /// Consume `e[+-]?digits` if present. Returns true if consumed.
    fn exponent(&mut self) -> bool {
        if !matches!(self.peek(0), Some(b'e') | Some(b'E')) {
            return false;
        }
        let save = self.pos;
        self.pos += 1;
        if matches!(self.peek(0), Some(b'+') | Some(b'-')) {
            self.pos += 1;
        }
        let ds = self.pos;
        while self.pos < self.text.len() && self.text[self.pos].is_ascii_digit() {
            self.pos += 1;
        }
        if self.pos == ds {
            self.pos = save;
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::SourceDb;

    fn lex_str(src: &str) -> (Vec<Token>, crate::source::SourceDb) {
        let mut db = SourceDb::new();
        let id = db.add_file("t.vhd", src).unwrap();
        let (toks, diags) = lex(id, db.file(id));
        assert!(diags.is_empty(), "{:?}", diags);
        (toks, db)
    }

    fn kinds(src: &str) -> Vec<(TokenKind, String)> {
        let (toks, db) = lex_str(src);
        toks.iter()
            .filter(|t| !matches!(t.kind, TokenKind::Whitespace | TokenKind::Eof))
            .map(|t| (t.kind, db.span_text(t.span).to_string()))
            .collect()
    }

    #[test]
    fn lexes_keywords_case_preserving() {
        let k = kinds("ENTITY Foo IS\nend;");
        assert_eq!(k[0], (TokenKind::Ident, "ENTITY".to_string()));
        assert_eq!(k[1], (TokenKind::Ident, "Foo".to_string()));
    }

    #[test]
    fn lexes_literals() {
        let k = kinds("'1' x\"FF\" 16#0F# 1_000 3.14 2#101#E1");
        assert_eq!(k[0], (TokenKind::CharLiteral, "'1'".to_string()));
        assert_eq!(k[1], (TokenKind::Ident, "x".to_string()));
        assert_eq!(k[2], (TokenKind::StrLiteral, "\"FF\"".to_string()));
        assert_eq!(k[3], (TokenKind::IntLiteral, "16#0F#".to_string()));
        assert_eq!(k[4], (TokenKind::IntLiteral, "1_000".to_string()));
        assert_eq!(k[5], (TokenKind::RealLiteral, "3.14".to_string()));
        assert_eq!(k[6], (TokenKind::IntLiteral, "2#101#E1".to_string()));
    }

    #[test]
    fn tick_vs_char() {
        let k = kinds("clk'event '0' a'range");
        assert_eq!(k[0], (TokenKind::Ident, "clk".to_string()));
        assert_eq!(k[1], (TokenKind::Punct, "'".to_string()));
        assert_eq!(k[2], (TokenKind::Ident, "event".to_string()));
        assert_eq!(k[3], (TokenKind::CharLiteral, "'0'".to_string()));
        assert_eq!(k[5], (TokenKind::Punct, "'".to_string()));
    }

    #[test]
    fn comments_and_strings() {
        let k = kinds("-- hello\nx <= \"a\"\"b\"; -- tail");
        assert_eq!(k[0], (TokenKind::Comment, "-- hello".to_string()));
        assert_eq!(k[4], (TokenKind::StrLiteral, "\"a\"\"b\"".to_string()));
    }
}
