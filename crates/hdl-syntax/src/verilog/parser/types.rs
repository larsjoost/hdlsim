//! User-defined type declarations: `typedef` (with optional enum/struct/
//! union bodies) and the enum/struct type grammars. Child module of
//! `verilog::parser`, the same pattern as `sva.rs`.

use crate::kinds::SyntaxKind;
use crate::token::TokenKind;

impl super::Parser<'_> {
    /// `typedef [enum|struct|union ...] name;` — the type body is parsed as
    /// EnumType/StructType when present.
    pub(super) fn parse_typedef(&mut self) {
        self.builder.start_node(SyntaxKind::Typedef);
        self.bump(); // typedef
        if self.at_kw("enum") {
            self.parse_enum_type();
        } else if self.at_kw("struct") || self.at_kw("union") {
            self.parse_struct_type();
        } else if self.at_kw("bit")
            || self.at_kw("logic")
            || self.at_kw("reg")
            || self.at_kw("byte")
            || self.at_kw("shortint")
            || self.at_kw("int")
            || self.at_kw("longint")
            || self.at_kw("integer")
            || self.at_kw("real")
            || self.at_kw("time")
            || self.at_kw("string")
            || self.at_kw("chandle")
            || self.at_kw("event")
        {
            self.bump();
            self.parse_optional_range();
        } else if self.peek().kind == TokenKind::Ident {
            // `typedef some_type name;` — copy of an existing type.
            self.builder.start_node(SyntaxKind::Name);
            self.bump();
            while self.at_punct('.') {
                self.bump();
                if self.peek().kind == TokenKind::Ident {
                    self.bump();
                } else {
                    break;
                }
            }
            self.builder.finish_node();
            self.parse_optional_range();
        }
        if self.peek().kind == TokenKind::Ident {
            self.builder.start_node(SyntaxKind::Name);
            let n = self.peek_text().to_string();
            self.bump();
            self.builder.finish_node();
            self.user_types.insert(n);
        } else {
            self.error_here("expected a typedef name");
        }
        self.expect_punct(';', "to end the typedef");
        self.builder.finish_node();
    }

    /// `enum [base_type] { A = 0, B, ... }` — base type optional.
    pub(super) fn parse_enum_type(&mut self) {
        self.builder.start_node(SyntaxKind::EnumType);
        self.bump(); // enum
        if self.peek().kind == TokenKind::Ident {
            self.builder.start_node(SyntaxKind::Name);
            self.bump(); // base type
            self.builder.finish_node();
            self.parse_optional_range();
        }
        self.expect_punct('{', "to open the enum body");
        loop {
            if self.at_punct('}') || self.peek().kind == TokenKind::Eof {
                break;
            }
            if self.peek().kind == TokenKind::Ident {
                self.bump_name_node();
            } else {
                self.error_here("expected an enum literal");
                break;
            }
            if self.eat_punct('=') {
                self.parse_expression();
            }
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct('}', "to close the enum body");
        self.builder.finish_node();
    }

    /// `struct/union [packed] [signed] { field; ... }`
    pub(super) fn parse_struct_type(&mut self) {
        let is_union = self.at_kw("union");
        self.builder.start_node(SyntaxKind::StructType);
        self.bump(); // struct | union
        if is_union {
            // keyword already consumed; `packed`/`signed` may follow either way
        }
        self.eat_kw("packed");
        self.eat_kw("signed");
        self.expect_punct('{', "to open the struct body");
        loop {
            if self.at_punct('}') || self.peek().kind == TokenKind::Eof {
                break;
            }
            // One field: `[modifiers] type [range] name [, name]* ;`
            while self.at_kw("rand") || self.at_kw("randc") || self.at_kw("const") {
                self.bump();
            }
            self.eat_type_words(); // `int unsigned`, `logic`, ...
                                   // Optional user type name for the field.
            if self.peek().kind == TokenKind::Ident
                && (self.peek2().kind == TokenKind::Ident || self.peek2_text() == "[")
            {
                self.bump();
            }
            if self.peek().kind != TokenKind::Ident && !self.at_punct('[') {
                self.error_here("expected a field type");
                break;
            }
            self.parse_optional_range();
            loop {
                if self.peek().kind == TokenKind::Ident {
                    self.builder.start_node(SyntaxKind::Name);
                    self.bump();
                    self.builder.finish_node();
                } else {
                    self.error_here("expected a field name");
                    break;
                }
                self.parse_optional_range();
                if !self.eat_punct(',') {
                    break;
                }
            }
            self.expect_punct(';', "to end the struct field");
        }
        self.expect_punct('}', "to close the struct body");
        self.builder.finish_node();
    }
}
