//! Token layer of the VHDL parser: trivia-skipping navigation, lookahead
//! ([`Parser::peek`] and friends), multi-char operator matching, keyword/
//! punctuation tests, token consumption (bump/eat/expect), and the
//! error-recovery helpers shared by every grammar module.
//!
//! Child module of `vhdl::parser`; methods are `pub(super)` so all grammar
//! modules use one token interface. VHDL keywords are matched
//! case-insensitively; the CST keeps the original text (lossless).

use crate::diagnostic::{Diagnostic, Severity};
use crate::kinds::SyntaxKind;
use crate::token::{Token, TokenKind};

impl super::Parser<'_> {
    pub(super) fn skip_trivia_from(&self, mut i: usize) -> usize {
        while i < self.tokens.len()
            && matches!(
                self.tokens[i].kind,
                TokenKind::Whitespace | TokenKind::Comment
            )
        {
            i += 1;
        }
        i
    }

    pub(super) fn peek(&self) -> &Token {
        // Clamp: at end of input this yields the trailing Eof token, never a panic.
        let i = self.skip_trivia_from(self.pos);
        &self.tokens[i.min(self.tokens.len() - 1)]
    }

    pub(super) fn peek2(&self) -> &Token {
        let i = self.skip_trivia_from(self.pos);
        let j = self.skip_trivia_from(i + 1);
        &self.tokens[j.min(self.tokens.len() - 1)]
    }

    pub(super) fn peek_text(&self) -> &str {
        let t = self.peek();
        &self.src.text()[t.span.start as usize..t.span.end as usize]
    }

    pub(super) fn peek2_text(&self) -> &str {
        let t = self.peek2();
        &self.src.text()[t.span.start as usize..t.span.end as usize]
    }

    pub(super) fn peek3_text(&self) -> &str {
        let i = self.skip_trivia_from(self.pos);
        let j = self.skip_trivia_from(i + 1);
        let k = self.skip_trivia_from(j + 1);
        let t = &self.tokens[k.min(self.tokens.len() - 1)];
        &self.src.text()[t.span.start as usize..t.span.end as usize]
    }

    pub(super) fn peek4_text(&self) -> &str {
        let i = self.skip_trivia_from(self.pos);
        let j = self.skip_trivia_from(i + 1);
        let k = self.skip_trivia_from(j + 1);
        let l = self.skip_trivia_from(k + 1);
        let t = &self.tokens[l.min(self.tokens.len() - 1)];
        &self.src.text()[t.span.start as usize..t.span.end as usize]
    }

    /// `ident :` starts a label — but `ident :=` is an assignment, so the
    /// colon must not be followed by `=`.
    pub(super) fn at_label(&self) -> bool {
        self.peek().kind == TokenKind::Ident && self.peek2_text() == ":" && self.peek3_text() != "="
    }

    pub(super) fn at_kw(&self, kw: &str) -> bool {
        let t = self.peek();
        t.kind == TokenKind::Ident && self.peek_text().eq_ignore_ascii_case(kw)
    }

    pub(super) fn at_punct(&self, c: char) -> bool {
        let t = self.peek();
        t.kind == TokenKind::Punct && t.span.len() == 1 && self.peek_text() == c.to_string()
    }

    /// Two adjacent single-char punct tokens forming a two-char sequence (`<<`, `>>`).
    pub(super) fn at_punct2(&self, s: &str) -> bool {
        let a = self.peek();
        let b = self.peek2();
        a.kind == TokenKind::Punct
            && b.kind == TokenKind::Punct
            && a.span.len() == 1
            && b.span.len() == 1
            && format!("{}{}", self.peek_text(), self.peek2_text()) == s
    }

    /// Three adjacent punct tokens forming a three-char operator (`?<=`, `?>=`).
    pub(super) fn peek_op3(&self) -> Option<String> {
        let i0 = self.skip_trivia_from(self.pos);
        let i1 = self.skip_trivia_from(i0 + 1);
        let i2 = self.skip_trivia_from(i1 + 1);
        if i2 >= self.tokens.len() {
            return None;
        }
        let ts: Vec<&Token> = [i0, i1, i2].iter().map(|&i| &self.tokens[i]).collect();
        if ts.iter().any(|t| t.kind != TokenKind::Punct) {
            return None;
        }
        let t = format!(
            "{}{}{}",
            &self.src.text()[ts[0].span.start as usize..ts[0].span.end as usize],
            &self.src.text()[ts[1].span.start as usize..ts[1].span.end as usize],
            &self.src.text()[ts[2].span.start as usize..ts[2].span.end as usize]
        );
        match t.as_str() {
            "?<=" | "?>=" | "?/=" => Some(t),
            _ => None,
        }
    }

    /// Two adjacent punct tokens forming a two-char operator (`<=`, `:=`, `=>`, `/=`, `**`, `<>`).
    pub(super) fn peek_op2(&self) -> Option<String> {
        let i0 = self.skip_trivia_from(self.pos);
        let i1 = self.skip_trivia_from(i0 + 1);
        if i0 >= self.tokens.len() || i1 >= self.tokens.len() {
            return None;
        }
        let (a, b) = (&self.tokens[i0], &self.tokens[i1]);
        if a.kind != TokenKind::Punct || b.kind != TokenKind::Punct {
            return None;
        }
        let t = format!(
            "{}{}",
            &self.src.text()[a.span.start as usize..a.span.end as usize],
            &self.src.text()[b.span.start as usize..b.span.end as usize]
        );
        match t.as_str() {
            "<=" | ":=" | "=>" | "/=" | "**" | "<>" | ">=" | "?=" | "?/=" | "?<" | "?>" => Some(t),
            _ => None,
        }
    }

    /// Emit pending trivia tokens into the currently open node and advance
    /// past them, so a node opened next starts at a real token.
    pub(super) fn bump_trivia(&mut self) {
        while self.pos < self.tokens.len() {
            let t = &self.tokens[self.pos];
            if matches!(t.kind, TokenKind::Whitespace | TokenKind::Comment) {
                self.builder
                    .token(SyntaxKind::from_token(t.kind), t.span.len());
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    /// Open a node at the next real token (skipping trivia first, so trivia
    /// never becomes a child of the new node).
    pub(super) fn start(&mut self, kind: SyntaxKind) {
        self.bump_trivia();
        self.builder.start_node(kind);
    }

    pub(super) fn bump(&mut self) -> Token {
        self.bump_trivia();
        if self.pos >= self.tokens.len() {
            // Past the trailing Eof: hand it out again without advancing.
            return self.tokens[self.tokens.len() - 1].clone();
        }
        let t = self.tokens[self.pos].clone();
        self.builder
            .token(SyntaxKind::from_token(t.kind), t.span.len());
        self.pos += 1;
        t
    }

    pub(super) fn eat_punct(&mut self, c: char) -> bool {
        if self.at_punct(c) {
            self.bump();
            true
        } else {
            false
        }
    }

    pub(super) fn eat_kw(&mut self, kw: &str) -> bool {
        if self.at_kw(kw) {
            self.bump();
            true
        } else {
            false
        }
    }

    pub(super) fn expect_punct(&mut self, c: char, ctx: &str) -> bool {
        if self.eat_punct(c) {
            true
        } else {
            self.error_here(&format!("expected '{}' {}", c, ctx));
            false
        }
    }

    pub(super) fn expect_kw(&mut self, kw: &str, ctx: &str) -> bool {
        if self.eat_kw(kw) {
            true
        } else {
            self.error_here(&format!("expected '{}' {}", kw, ctx));
            false
        }
    }

    pub(super) fn expect_ident(&mut self, ctx: &str) -> Option<String> {
        if self.peek().kind == TokenKind::Ident {
            let t = self.peek_text().to_string();
            self.bump();
            Some(t)
        } else {
            self.error_here(&format!("expected an identifier {}", ctx));
            None
        }
    }

    pub(super) fn error_here(&mut self, msg: &str) {
        let t = self.peek().clone();
        self.diags.push(Diagnostic {
            code: "E0301",
            severity: Severity::Error,
            span: Some(t.span),
            message: msg.to_string(),
            help: None,
        });
    }

    pub(super) fn skip_to_semicolon(&mut self) {
        while self.peek().kind != TokenKind::Eof && !self.at_punct(';') {
            self.bump();
        }
        self.eat_punct(';');
    }
}
