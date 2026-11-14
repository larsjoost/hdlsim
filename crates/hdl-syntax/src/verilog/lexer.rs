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
                b'$' => self.sys_task(),
                b'`' => self.directive(),
                b'\'' => self.based_literal(),
                b'0'..=b'9' => self.number(),
                _ if is_ident_start(b) => {
                    self.run_of(TokenKind::Ident, is_ident_continue);
                }
                _ if b.is_ascii() => {
                    // Single-character punctuation.
                    self.emit(TokenKind::Punct, 1);
                }
                _ => {
                    self.diags.push(
                        Diagnostic::error(
                            "E0101",
                            Some(Span::new(self.file, self.pos as u32, self.pos as u32 + 1)),
                            format!("unexpected non-ASCII byte 0x{:02x}", b),
                        )
                        .with_help("HDL sources are expected to be ASCII"),
                    );
                    self.emit(TokenKind::Error, 1);
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
                Some(b'\\') => self.pos += 2, // escape: skip the escaped byte
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
        if self.peek(0).map_or(false, is_ident_start) {
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
        if self.peek(0).map_or(false, is_ident_start) {
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

    fn based_literal(&mut self) {
        // `' [sS]? [bBoOdDhH] value` — a based literal without explicit size.
        let start = self.pos;
        self.pos += 1; // `'`
        let mut ok = false;
        if matches!(self.peek(0), Some(b's') | Some(b'S')) {
            self.pos += 1;
        }
        if matches!(
            self.peek(0),
            Some(b'b')
                | Some(b'B')
                | Some(b'o')
                | Some(b'O')
                | Some(b'd')
                | Some(b'D')
                | Some(b'h')
                | Some(b'H')
        ) {
            self.pos += 1;
            ok = true;
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
        if ok {
            self.tokens.push(Token::new(
                TokenKind::IntLiteral,
                Span::new(self.file, start as u32, self.pos as u32),
            ));
        } else {
            // Lone `'` — not valid Verilog outside a based literal.
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
    }

    fn number(&mut self) {
        // `[size]'[sS]?base value` | decimal | real (`1.5`, `1e-3`).
        let start = self.pos;
        while self.pos < self.text.len()
            && (is_digit(self.text[self.pos]) || self.text[self.pos] == b'_')
        {
            self.pos += 1;
        }
        // Based literal with explicit size: `16'hFF`.
        if self.peek(0) == Some(b'\'') {
            let after_quote = self.peek(1);
            let base_ahead = match after_quote {
                Some(b's') | Some(b'S') => self.peek(2),
                _ => after_quote,
            };
            if matches!(
                base_ahead,
                Some(b'b')
                    | Some(b'B')
                    | Some(b'o')
                    | Some(b'O')
                    | Some(b'd')
                    | Some(b'D')
                    | Some(b'h')
                    | Some(b'H')
            ) {
                self.pos += 1; // `'`
                if matches!(self.peek(0), Some(b's') | Some(b'S')) {
                    self.pos += 1;
                }
                self.pos += 1; // base char
                while self.pos < self.text.len()
                    && (is_digit(self.text[self.pos])
                        || matches!(
                            self.text[self.pos],
                            b'a'..=b'f' | b'A'..=b'F' | b'_' | b'x' | b'X' | b'z' | b'Z' | b'?'
                        ))
                {
                    self.pos += 1;
                }
                self.tokens.push(Token::new(
                    TokenKind::IntLiteral,
                    Span::new(self.file, start as u32, self.pos as u32),
                ));
                return;
            }
        }
        // Real literal: digits '.' digits [exponent] | digits exponent.
        let mut is_real = false;
        if self.peek(0) == Some(b'.') && self.peek(1).map_or(false, |b| is_digit(b) || b == b'_') {
            is_real = true;
            self.pos += 1;
            while self.pos < self.text.len()
                && (is_digit(self.text[self.pos]) || self.text[self.pos] == b'_')
            {
                self.pos += 1;
            }
        }
        if matches!(self.peek(0), Some(b'e') | Some(b'E')) {
            let exp_start = self.pos;
            self.pos += 1;
            if matches!(self.peek(0), Some(b'+') | Some(b'-')) {
                self.pos += 1;
            }
            if self.peek(0).map_or(false, is_digit) {
                is_real = true;
                while self.pos < self.text.len() && is_digit(self.text[self.pos]) {
                    self.pos += 1;
                }
            } else {
                self.pos = exp_start; // not an exponent after all
            }
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
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tokens carry only spans, so the SourceFile can be dropped after lexing.
    fn lex_str(src: &str) -> (Vec<Token>, Vec<Diagnostic>) {
        let file = SourceFile::new("t.v", src);
        lex(FileId(0), &file)
    }

    fn kinds(src: &str) -> Vec<TokenKind> {
        lex_str(src).0.iter().map(|t| t.kind).collect()
    }

    #[test]
    fn lexes_module_header() {
        let ks = kinds("module m;");
        assert_eq!(
            ks,
            vec![
                TokenKind::Ident,
                TokenKind::Whitespace,
                TokenKind::Ident,
                TokenKind::Punct,
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn lexes_comments_and_strings() {
        let ks = kinds("// hi\n/* x */ \"a\\\"b\"");
        assert_eq!(
            ks,
            vec![
                TokenKind::Comment,
                TokenKind::Whitespace,
                TokenKind::Comment,
                TokenKind::Whitespace,
                TokenKind::StrLiteral,
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn lexes_sys_task_directive_and_literals() {
        let ks = kinds("$display(`WIDTH, 16'hFF, 'b10, 3.14, 1e-3)");
        assert_eq!(
            ks,
            vec![
                TokenKind::SysTask,
                TokenKind::Punct,
                TokenKind::Directive,
                TokenKind::Punct,
                TokenKind::Whitespace,
                TokenKind::IntLiteral,
                TokenKind::Punct,
                TokenKind::Whitespace,
                TokenKind::IntLiteral,
                TokenKind::Punct,
                TokenKind::Whitespace,
                TokenKind::RealLiteral,
                TokenKind::Punct,
                TokenKind::Whitespace,
                TokenKind::RealLiteral,
                TokenKind::Punct,
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn token_spans_reconstruct_source() {
        let src = "module m; // c\nendmodule";
        let (tokens, diags) = lex_str(src);
        assert!(diags.is_empty());
        let mut recon = String::new();
        let file = SourceFile::new("t.v", src);
        for t in &tokens {
            if t.kind == TokenKind::Eof {
                continue;
            }
            recon.push_str(&file.text()[t.span.start as usize..t.span.end as usize]);
        }
        assert_eq!(recon, src);
    }

    #[test]
    fn unterminated_string_is_a_diagnostic_not_a_panic() {
        let (_, diags) = lex_str("\"oops");
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("unterminated string"));
    }

    #[test]
    fn never_panics_on_arbitrary_bytes() {
        // Fuzz-lite: every single byte and some slices must lex without panic.
        for b in 0u16..=255 {
            let _ = lex_str(&String::from_utf8_lossy(&[b as u8]));
        }
        let _ = lex_str("\u{1F600} \u{0} $ ` ' \" /*");
    }
}
