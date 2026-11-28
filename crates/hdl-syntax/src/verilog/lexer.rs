//! Verilog lexer: source text → tokens + diagnostics.
//!
//! Byte-oriented and panic-free: every unexpected byte becomes an `Error` token
//! plus a diagnostic. Multi-character operators (`<=`, `==`, `<<`, ...) are *not*
//! combined here — the lexer emits single `Punct` tokens and the parser combines
//! them, which keeps this file small and the grammar mapping explicit.

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
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

fn is_digit(b: u8) -> bool {
    b.is_ascii_digit()
}

/// A base-specifier byte (`b`/`o`/`d`/`h`, case-insensitive).
fn is_base_char(b: Option<u8>) -> bool {
    matches!(
        b,
        Some(b'b')
            | Some(b'B')
            | Some(b'o')
            | Some(b'O')
            | Some(b'd')
            | Some(b'D')
            | Some(b'h')
            | Some(b'H')
    )
}

impl<'a> Lexer<'a> {
    fn run(&mut self) {
        while self.pos < self.text.len() {
            let b = self.text[self.pos];
            match b {
                b' ' | b'\t' | b'\r' | b'\n' | 0x0b | 0x0c => self
                    .run_of(TokenKind::Whitespace, |b| {
                        matches!(b, b' ' | b'\t' | b'\r' | b'\n' | 0x0b | 0x0c)
                    }),
                b'/' if self.peek(1) == Some(b'/') => self.line_comment(),
                b'/' if self.peek(1) == Some(b'*') => self.block_comment(),
                b'"' => self.string(),
                b'$' if self.peek(1) == Some(b']') => self.emit(TokenKind::Punct, 1), // `q[$]`
                b'$' => self.sys_task(),
                b'`' => self.directive(),
                b'\'' if self.peek(1) == Some(b'{') => self.emit(TokenKind::Punct, 1), // `'{...}`
                b'\'' => self.based_literal(),
                b'\\' => self.escaped_ident(),
                b'0'..=b'9' => self.number(),
                _ if is_ident_start(b) => {
                    self.run_of(TokenKind::Ident, is_ident_continue);
                }
                _ if b.is_ascii() => {
                    // Single-character punctuation.
                    self.emit(TokenKind::Punct, 1);
                }
                _ => {
                    // Consume the whole UTF-8 char so token spans stay on char
                    // boundaries (slicing mid-char would panic in span_text).
                    let clen = match b {
                        0xC0..=0xDF => 2,
                        0xE0..=0xEF => 3,
                        _ => 4,
                    }
                    .min(self.text.len() - self.pos);
                    self.diags.push(
                        Diagnostic::error(
                            "E0101",
                            Some(Span::new(
                                self.file,
                                self.pos as u32,
                                self.pos as u32 + clen as u32,
                            )),
                            format!("unexpected non-ASCII byte 0x{:02x}", b),
                        )
                        .with_help("HDL sources are expected to be ASCII"),
                    );
                    self.emit(TokenKind::Error, clen as u32);
                }
            }
        }
    }

    fn peek(&self, ahead: usize) -> Option<u8> {
        self.text.get(self.pos + ahead).copied()
    }

    /// Consume a maximal run of bytes satisfying `pred` as one token.
    /// Advances `pos` itself; do not call `emit` afterwards.
    fn run_of(&mut self, kind: TokenKind, pred: impl Fn(u8) -> bool) {
        let start = self.pos;
        while self.pos < self.text.len() && pred(self.text[self.pos]) {
            self.pos += 1;
        }
        self.tokens.push(Token::new(
            kind,
            Span::new(self.file, start as u32, self.pos as u32),
        ));
    }

    /// Emit a token covering `len` bytes starting at the pre-call position.
    /// Callers advance `pos` themselves where needed; here we always advance.
    fn emit(&mut self, kind: TokenKind, len: u32) {
        let start = self.pos;
        self.pos += len as usize;
        self.tokens.push(Token::new(
            kind,
            Span::new(self.file, start as u32, self.pos as u32),
        ));
    }

    fn line_comment(&mut self) {
        let start = self.pos;
        while self.pos < self.text.len() && self.text[self.pos] != b'\n' {
            self.pos += 1;
        }
        self.tokens.push(Token::new(
            TokenKind::Comment,
            Span::new(self.file, start as u32, self.pos as u32),
        ));
    }

    fn block_comment(&mut self) {
        let start = self.pos;
        self.pos += 2; // "/*"
        loop {
            match self.peek(0) {
                None => {
                    self.diags.push(
                        Diagnostic::error(
                            "E0102",
                            Some(Span::new(self.file, start as u32, self.pos as u32)),
                            "unterminated block comment",
                        )
                        .with_help("add `*/` to close the comment"),
                    );
                    break;
                }
                Some(b'*') if self.peek(1) == Some(b'/') => {
                    self.pos += 2;
                    break;
                }
                _ => self.pos += 1,
            }
        }
        self.tokens.push(Token::new(
            TokenKind::Comment,
            Span::new(self.file, start as u32, self.pos as u32),
        ));
    }

    fn string(&mut self) {
        let start = self.pos;
        self.pos += 1; // opening quote
        loop {
            match self.peek(0) {
                None | Some(b'\n') => {
                    self.diags.push(
                        Diagnostic::error(
                            "E0103",
                            Some(Span::new(self.file, start as u32, self.pos as u32)),
                            "unterminated string literal",
                        )
                        .with_help("add a closing `\"`"),
                    );
                    break;
                }
                // escape: skip the escaped byte, clamped so a trailing `\`
                // at EOF cannot push pos past the end of the text
                Some(b'\\') => self.pos = (self.pos + 2).min(self.text.len()),
                Some(b'"') => {
                    self.pos += 1;
                    break;
                }
                _ => self.pos += 1,
            }
        }
        self.tokens.push(Token::new(
            TokenKind::StrLiteral,
            Span::new(self.file, start as u32, self.pos as u32),
        ));
    }

    fn sys_task(&mut self) {
        // `$` followed by identifier characters.
        let start = self.pos;
        self.pos += 1;
        if self.peek(0).is_some_and(is_ident_start) {
            while self.pos < self.text.len() && is_ident_continue(self.text[self.pos]) {
                self.pos += 1;
            }
            self.tokens.push(Token::new(
                TokenKind::SysTask,
                Span::new(self.file, start as u32, self.pos as u32),
            ));
        } else {
            self.diags.push(
                Diagnostic::error(
                    "E0104",
                    Some(Span::new(self.file, start as u32, start as u32 + 1)),
                    "`$` must be followed by a system task name",
                )
                .with_help("e.g. $display, $finish"),
            );
            self.tokens.push(Token::new(
                TokenKind::Error,
                Span::new(self.file, start as u32, start as u32 + 1),
            ));
        }
    }

    fn directive(&mut self) {
        // `` ` `` followed by identifier characters (macro reference / directive).
        let start = self.pos;
        self.pos += 1;
        if self.peek(0).is_some_and(is_ident_start) {
            while self.pos < self.text.len() && is_ident_continue(self.text[self.pos]) {
                self.pos += 1;
            }
            self.tokens.push(Token::new(
                TokenKind::Directive,
                Span::new(self.file, start as u32, self.pos as u32),
            ));
        } else {
            self.diags.push(
                Diagnostic::error(
                    "E0105",
                    Some(Span::new(self.file, start as u32, start as u32 + 1)),
                    "`` ` `` must be followed by a directive name",
                )
                .with_help("e.g. `define, `include"),
            );
            self.tokens.push(Token::new(
                TokenKind::Error,
                Span::new(self.file, start as u32, start as u32 + 1),
            ));
        }
    }

    /// Escaped identifier: `\` + a run of non-whitespace bytes. The terminating
    /// whitespace is *not* part of the token (IEEE 1364: escaped identifier ends
    /// at the first whitespace). Emitted as one Ident token; the backslash stays
    /// in the text, keeping the CST lossless.
    fn escaped_ident(&mut self) {
        let start = self.pos;
        self.pos += 1; // `\`
        while self.pos < self.text.len() && !self.text[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
        self.tokens.push(Token::new(
            TokenKind::Ident,
            Span::new(self.file, start as u32, self.pos as u32),
        ));
    }

    fn based_literal(&mut self) {
        // `' [sS]? [bBoOdDhH] value` — a based literal without explicit size,
        // or an unsized `'0 '1 'x 'z 'X 'Z '?'.
        let start = self.pos;
        self.pos += 1; // `'`
        if self.unsized_value_ahead() {
            self.pos += 1;
            self.tokens.push(Token::new(
                TokenKind::IntLiteral,
                Span::new(self.file, start as u32, self.pos as u32),
            ));
            return;
        }
        if self.peek(0) == Some(b'(') {
            // Cast tick: `signed'(x)` / `8'(x)` — a lone `'` is punctuation here.
            self.tokens.push(Token::new(
                TokenKind::Punct,
                Span::new(self.file, start as u32, start as u32 + 1),
            ));
            return;
        }
        if matches!(self.peek(0), Some(b's') | Some(b'S')) {
            self.pos += 1;
        }
        if is_base_char(self.peek(0)) {
            self.pos += 1; // base char
            self.eat_base_digits();
            self.tokens.push(Token::new(
                TokenKind::IntLiteral,
                Span::new(self.file, start as u32, self.pos as u32),
            ));
            return;
        }
        self.lone_tick_error(start);
    }

    /// An unsized `'0`/`'1`/`'x`/`'z`/`'?` directly after the tick.
    fn unsized_value_ahead(&self) -> bool {
        matches!(
            self.peek(0),
            Some(b'0')
                | Some(b'1')
                | Some(b'x')
                | Some(b'X')
                | Some(b'z')
                | Some(b'Z')
                | Some(b'?')
        )
    }

    /// A lone `'` — not valid Verilog outside a based literal: diagnostic
    /// and an Error token, so lexing continues after it.
    fn lone_tick_error(&mut self, start: usize) {
        self.diags.push(
            Diagnostic::error(
                "E0106",
                Some(Span::new(self.file, start as u32, start as u32 + 1)),
                "expected a base specifier after `'`",
            )
            .with_help("valid bases: 'b 'o 'd 'h (optionally signed: 'sb …)"),
        );
        self.tokens.push(Token::new(
            TokenKind::Error,
            Span::new(self.file, start as u32, start as u32 + 1),
        ));
    }

    fn number(&mut self) {
        // `[size]'[sS]?base value` | decimal | real (`1.5`, `1e-3`).
        let start = self.pos;
        self.eat_dec_digits();
        if self.peek(0) == Some(b'\'') && is_base_char(self.sized_base_ahead()) {
            self.number_based(start);
            return;
        }
        self.number_decimal_or_real(start);
    }

    /// The base byte after the current `'` (skipping an `s`/`S` sign), or
    /// None when no base specifier follows.
    fn sized_base_ahead(&self) -> Option<u8> {
        let after_quote = self.peek(1);
        match after_quote {
            Some(b's') | Some(b'S') => self.peek(2),
            _ => after_quote,
        }
    }

    /// `[size]'[sS]?base value` with the size digits already consumed: the
    /// `'`, optional sign, base char, and value digits form one IntLiteral.
    fn number_based(&mut self, start: usize) {
        self.pos += 1; // `'`
        if matches!(self.peek(0), Some(b's') | Some(b'S')) {
            self.pos += 1;
        }
        self.pos += 1; // base char
        self.eat_base_digits();
        self.tokens.push(Token::new(
            TokenKind::IntLiteral,
            Span::new(self.file, start as u32, self.pos as u32),
        ));
    }

    /// Decimal or real (`1.5`, `1.5e-3`, `1e3`) with the leading digits
    /// already consumed.
    fn number_decimal_or_real(&mut self, start: usize) {
        let mut is_real = self.fraction_digits();
        if self.exponent_digits() {
            is_real = true;
        }
        let kind = if is_real {
            TokenKind::RealLiteral
        } else {
            TokenKind::IntLiteral
        };
        self.tokens.push(Token::new(
            kind,
            Span::new(self.file, start as u32, self.pos as u32),
        ));
    }

    /// `.` digits — consumes the fraction and returns true when present.
    fn fraction_digits(&mut self) -> bool {
        if self.peek(0) == Some(b'.') && self.peek(1).is_some_and(|b| is_digit(b) || b == b'_') {
            self.pos += 1;
            self.eat_dec_digits();
            return true;
        }
        false
    }

    /// `[eE] [+-] digits` — consumes a full exponent and returns true; a
    /// bare `[eE]` without digits is left in place (it ends the number).
    fn exponent_digits(&mut self) -> bool {
        if !matches!(self.peek(0), Some(b'e') | Some(b'E')) {
            return false;
        }
        let exp_start = self.pos;
        self.pos += 1;
        if matches!(self.peek(0), Some(b'+') | Some(b'-')) {
            self.pos += 1;
        }
        if self.peek(0).is_some_and(is_digit) {
            while self.pos < self.text.len() && is_digit(self.text[self.pos]) {
                self.pos += 1;
            }
            return true;
        }
        self.pos = exp_start; // not an exponent after all
        false
    }

    /// Decimal digits and `_` separators (`1_000`).
    fn eat_dec_digits(&mut self) {
        while self.pos < self.text.len()
            && (is_digit(self.text[self.pos]) || self.text[self.pos] == b'_')
        {
            self.pos += 1;
        }
    }

    /// The value digits of a based literal: hex digits, `_`, and the x/z/?
    /// unknown states.
    fn eat_base_digits(&mut self) {
        while self.pos < self.text.len()
            && (is_digit(self.text[self.pos])
                || matches!(
                    self.text[self.pos],
                    b'a'..=b'f' | b'A'..=b'F' | b'_' | b'x' | b'X' | b'z' | b'Z' | b'?'
                ))
        {
            self.pos += 1;
        }
    }
}

#[cfg(test)]
mod tests;
