//! Names: external names (VHDL-2008 `<< signal path : type >>`), external
//! paths, plain/selected names, postfix names, and the suffix chain —
//! call/index/slice parens, selected names, bracketed subprogram
//! signatures, qualified expressions, and attribute designators.
//!
//! Child module of `vhdl::parser`, the same pattern as `decl.rs`.

use crate::kinds::SyntaxKind;
use crate::token::TokenKind;

impl super::Parser<'_> {
    /// A name: `ident` with selected (`.b`), indexed (`(i)`), sliced
    /// (`(msb downto lsb)`), called (`(args)`), or attributed (`'attr`) suffixes.
    /// VHDL-2008 external name: `<< signal|constant|variable path : subtype >>`.
    pub(super) fn parse_ext_name(&mut self) {
        self.start(SyntaxKind::ExtName);
        self.bump();
        self.bump(); // <<
        if self.peek().kind == TokenKind::Ident {
            self.bump(); // signal | constant | variable
        }
        self.parse_external_path();
        self.expect_punct(':', "in the external name");
        self.parse_type_mark();
        if self.at_punct('(') {
            self.parse_constraint();
        }
        if self.at_punct2(">>") {
            self.bump();
            self.bump(); // >>
        } else {
            self.error_here("expected '>>' to close the external name");
        }
        self.builder.finish_node();
    }

    /// External-name path: `[^] [. name]...` with optional indices and a
    /// trailing `.all` (e.g. `^.Transmitter_1.TransRec`, `dut.bus.all`).
    pub(super) fn parse_external_path(&mut self) {
        self.start(SyntaxKind::Name);
        if self.at_punct('^') {
            self.bump();
        }
        if self.at_punct('.') {
            self.bump();
        }
        loop {
            if matches!(
                self.peek().kind,
                TokenKind::Ident | TokenKind::StrLiteral | TokenKind::CharLiteral
            ) {
                self.bump();
            } else {
                self.error_here("expected a name in the external path");
                break;
            }
            self.skip_balanced('(', ')');
            if self.at_punct('.') {
                self.bump();
                if self.at_kw("all") {
                    self.bump();
                    break; // trailing `.all`
                }
                continue;
            }
            break;
        }
        self.builder.finish_node();
    }

    pub(super) fn parse_name(&mut self) {
        let mark = self.builder.mark();
        if matches!(
            self.peek().kind,
            TokenKind::Ident | TokenKind::StrLiteral | TokenKind::CharLiteral
        ) {
            self.start(SyntaxKind::Name);
            self.bump();
            while self.at_punct('.') {
                self.bump();
                // Selected name may be a quoted operator: `pkg."+"(a, b)`.
                if matches!(
                    self.peek().kind,
                    TokenKind::Ident | TokenKind::StrLiteral | TokenKind::CharLiteral
                ) {
                    self.bump();
                } else {
                    self.error_here("expected a name after '.'");
                    break;
                }
            }
            self.builder.finish_node();
        } else {
            self.error_here("expected a name");
            self.bump();
            return;
        }
        self.parse_name_suffixes(mark);
    }

    pub(super) fn parse_postfix_name(&mut self) {
        self.parse_name();
    }

    pub(super) fn parse_name_suffixes(&mut self, mark: crate::cst::Mark) {
        loop {
            // VHDL-2019 generic map on a subprogram call:
            // `f generic map (T => u) (args);` — skip the map, then the args.
            if self.at_kw("generic") && self.peek2_text().eq_ignore_ascii_case("map") {
                self.bump();
                self.bump();
                self.parse_assoc_list();
                continue;
            }
            if self.at_punct('(') {
                self.parse_call_or_slice_suffix(mark);
            } else if self.at_punct('.') {
                // On a malformed selector the old code left the loop without
                // wrapping a Name node; the bool return preserves that.
                if !self.parse_selected_suffix(mark) {
                    break;
                }
            } else if self.at_punct('[') {
                // Subprogram signature in brackets: `f[return T]'encoding`
                // — skip the balanced `[...]`, then keep looping so a
                // following `'attr` suffix is parsed too.
                self.skip_balanced('[', ']');
            } else if self.at_punct('\'') {
                if self.peek2().kind == TokenKind::Punct && self.peek2_text() == "(" {
                    self.parse_qualified_suffix(mark);
                } else {
                    self.parse_attr_suffix(mark);
                }
            } else {
                break;
            }
        }
    }

    /// `(...)` suffix: call arguments, index list, or a slice. Args/indices
    /// are `expr [, expr]*` where each may be a named association
    /// `name => expr`; a single `expr to|downto expr` makes it a slice.
    pub(super) fn parse_call_or_slice_suffix(&mut self, mark: crate::cst::Mark) {
        self.bump(); // (
        let mut is_slice = false;
        if !self.at_punct(')') {
            loop {
                if self.assoc_is_named() {
                    // Named association; the formal may be indexed:
                    // `a(0) => b`. Bump the formal's tokens without
                    // wrapping them in a Name node so the lowerer keeps
                    // treating it as a skipped formal, not an argument.
                    self.skip_assoc_formal();
                    self.bump();
                    self.bump(); // =>
                }
                self.parse_expression();
                if self.at_kw("to") || self.at_kw("downto") {
                    is_slice = true;
                    self.bump(); // to / downto
                    self.parse_expression();
                }
                if !self.eat_punct(',') {
                    break;
                }
            }
        }
        self.expect_punct(')', "to close the argument/index/slice list");
        let kind = if is_slice {
            SyntaxKind::PartSelect
        } else {
            SyntaxKind::Call
        };
        self.builder.start_node_at(mark, kind);
        self.builder.finish_node();
    }

    /// `arr(i).field` — a selected name after a suffix: fold the whole
    /// `name(args).field` into one Name node (the Call stays nested inside
    /// it, so the lowerer can see both parts). Returns false on a malformed
    /// selector, where no Name node is wrapped and the suffix loop ends.
    pub(super) fn parse_selected_suffix(&mut self, mark: crate::cst::Mark) -> bool {
        self.bump();
        if matches!(
            self.peek().kind,
            TokenKind::Ident | TokenKind::StrLiteral | TokenKind::CharLiteral
        ) {
            self.bump();
        } else {
            self.error_here("expected a name after '.'");
            return false;
        }
        self.builder.start_node_at(mark, SyntaxKind::Name);
        self.builder.finish_node();
        true
    }

    /// Qualified expression: `type_mark'(expr)`. The operand may be an
    /// aggregate: `integer_vector'(1, 7)`, `rec_t'(ID => x)`,
    /// `s'(1 to N => ' ')`.
    pub(super) fn parse_qualified_suffix(&mut self, mark: crate::cst::Mark) {
        self.bump(); // '
        self.builder.start_node_at(mark, SyntaxKind::Qualified);
        self.bump(); // (
        self.parse_expression();
        loop {
            if self.peek_op2().as_deref() == Some("=>") {
                self.bump();
                self.bump();
                self.parse_expression();
            } else if self.at_kw("to") || self.at_kw("downto") {
                self.bump();
                self.parse_expression();
            } else if self.eat_punct(',') {
                self.parse_agg_element();
            } else {
                break;
            }
        }
        self.expect_punct(')', "to close the qualified expression");
        self.builder.finish_node();
    }

    /// Attribute designator: `name'attr` / `name'val` (also a char literal).
    pub(super) fn parse_attr_suffix(&mut self, mark: crate::cst::Mark) {
        self.bump(); // '
        if matches!(self.peek().kind, TokenKind::Ident | TokenKind::CharLiteral) {
            self.builder.start_node_at(mark, SyntaxKind::Attr);
            self.bump(); // attribute name / char literal
            self.builder.finish_node();
        } else {
            self.error_here("expected an attribute name after the tick");
        }
    }
}
