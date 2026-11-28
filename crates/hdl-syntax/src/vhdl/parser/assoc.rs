//! Association lists: `( assoc {, assoc} )` for generic/port maps and
//! aggregate-like arguments, where each association is `.name => expr`,
//! `name => expr` (the formal possibly indexed/sliced/selected), or a
//! positional expression (`<>` is the open box).
//!
//! [`Parser::assoc_is_named`] is a pure token lookahead used by callers that
//! must decide before consuming; it never touches the builder.
//!
//! Child module of `vhdl::parser`, the same pattern as `design.rs`.

use crate::kinds::SyntaxKind;
use crate::token::TokenKind;

impl super::Parser<'_> {
    /// `( assoc {, assoc} )` where assoc is `.name => expr`, `name => expr`,
    /// or a positional expression.
    /// Is the association at the current position named (`formal => actual`)?
    /// The formal may be a plain name, an indexed/sliced name (`BitsA(0) =>`),
    /// or a selected name (`x.y(2) =>`).
    pub(super) fn assoc_is_named(&self) -> bool {
        let i = self.skip_trivia_from(self.pos);
        // The formal may also be an operator symbol: `"<"(a => b, ...)`.
        if !self
            .tokens
            .get(i)
            .is_some_and(|t| matches!(t.kind, TokenKind::Ident | TokenKind::StrLiteral))
        {
            return false;
        }
        let Some(i) = self.assoc_formal_end(self.skip_trivia_from(i + 1)) else {
            return false;
        };
        let a = self.tokens.get(i);
        let b = self.tokens.get(self.skip_trivia_from(i + 1));
        matches!((a, b), (Some(x), Some(y)) if x.kind == TokenKind::Punct && y.kind == TokenKind::Punct
            && &self.src.text()[x.span.start as usize..x.span.end as usize] == "="
            && &self.src.text()[y.span.start as usize..y.span.end as usize] == ">")
    }

    /// Lookahead cursor just past an association formal: the leading name
    /// plus any `(...)` / `[...]` suffixes and `.name` selects. `None` when
    /// the token run ends mid-formal.
    pub(super) fn assoc_formal_end(&self, mut i: usize) -> Option<usize> {
        loop {
            let t = self.tokens.get(i)?;
            let txt = &self.src.text()[t.span.start as usize..t.span.end as usize];
            if txt == "(" || txt == "[" {
                let close = if txt == "(" { ")" } else { "]" };
                i = self.assoc_bracket_end(i, txt, close)?;
            } else if txt == "." {
                i = self.skip_trivia_from(i + 1);
                if self
                    .tokens
                    .get(i)
                    .is_some_and(|t| t.kind == TokenKind::Ident)
                {
                    i = self.skip_trivia_from(i + 1);
                } else {
                    return None;
                }
            } else {
                return Some(i);
            }
        }
    }

    /// Lookahead cursor just past a balanced `(...)` / `[...]` group starting
    /// at `i`, trivia after it skipped. `None` on end-of-input mid-group.
    pub(super) fn assoc_bracket_end(&self, mut i: usize, open: &str, close: &str) -> Option<usize> {
        let mut depth = 0usize;
        loop {
            let t = self.tokens.get(i)?;
            let x = &self.src.text()[t.span.start as usize..t.span.end as usize];
            if x == open {
                depth += 1;
            } else if x == close {
                depth -= 1;
                if depth == 0 {
                    return Some(self.skip_trivia_from(i + 1));
                }
            }
            i += 1;
        }
    }

    /// Bump the tokens of a named-association formal (`name`, possibly with
    /// `(...)` / `.name` suffixes) without wrapping them in a node.
    pub(super) fn skip_assoc_formal(&mut self) {
        if matches!(self.peek().kind, TokenKind::Ident | TokenKind::StrLiteral) {
            self.bump();
        }
        loop {
            let txt = self.peek_text().to_string();
            if txt == "(" {
                let mut depth = 0usize;
                while self.peek().kind != TokenKind::Eof {
                    let t = self.peek_text().to_string();
                    self.bump();
                    if t == "(" {
                        depth += 1;
                    } else if t == ")" {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                }
            } else if txt == "." {
                self.bump();
                if self.peek().kind == TokenKind::Ident {
                    self.bump();
                }
            } else {
                break;
            }
        }
    }

    pub(super) fn parse_assoc_list(&mut self) {
        self.expect_punct('(', "to open the association list");
        while !self.at_punct(')') && self.peek().kind != TokenKind::Eof {
            self.start(SyntaxKind::PortConn);
            let named = self.at_punct('.') || self.assoc_is_named();
            if named {
                if self.eat_punct('.') {
                    let m = self.builder.mark();
                    if self.peek().kind == TokenKind::Ident {
                        self.bump();
                    }
                    self.builder.start_node_at(m, SyntaxKind::Name);
                    self.builder.finish_node();
                } else {
                    // Formal may be an indexed/sliced name: `BitsA(0) => x`.
                    self.parse_name();
                }
                if self.peek_op2().as_deref() == Some("=>") {
                    self.bump();
                    self.bump();
                } else {
                    self.error_here("expected '=>' in the association");
                }
            }
            if !self.at_punct(')') {
                if self.peek_text() == "<" && self.peek2_text() == ">" {
                    self.bump();
                    self.bump(); // `<>` box (open association)
                } else {
                    self.parse_expression();
                }
            }
            self.builder.finish_node();
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct(')', "to close the association list");
    }
}
