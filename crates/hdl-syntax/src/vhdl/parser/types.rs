//! Type declarations and constraints: type declarations of every kind
//! (incomplete, VHDL-2019 instantiations, enumerations, physical
//! `units`, arrays, records, access, file, protected incl. bodies),
//! subtypes with resolution functions, type marks, and index/range
//! constraints.
//!
//! Child module of `vhdl::parser`, the same pattern as `decl.rs`.

use crate::kinds::SyntaxKind;
use crate::token::TokenKind;

impl super::Parser<'_> {
    pub(super) fn parse_type_decl(&mut self) {
        self.start(SyntaxKind::TypeDecl);
        self.bump(); // type
        self.expect_ident("for the type name");
        // Incomplete (forward) type declaration: `type ListType;`
        if self.eat_punct(';') {
            self.builder.finish_node();
            return;
        }
        self.expect_kw("is", "after the type name");
        if self.at_kw("new") {
            self.parse_type_instantiation();
        } else if self.at_punct('(') {
            self.parse_enum_type();
        } else if self.at_kw("range") {
            self.bump();
            self.parse_range();
            if self.at_kw("units") {
                self.parse_units();
            }
        } else if self.at_kw("array") {
            self.parse_array_type();
        } else if self.at_kw("record") {
            self.parse_record_type();
        } else if self.at_kw("access") {
            // `access type_mark [constraint]` — pointer type.
            self.bump();
            self.parse_type_mark();
            if self.at_punct('(') {
                self.parse_constraint();
            }
        } else if self.at_kw("file") {
            // `file of type_mark` — file type.
            self.bump();
            self.expect_kw("of", "in the file type");
            self.parse_type_mark();
        } else if self.at_kw("protected") {
            self.parse_protected_type();
        } else {
            self.error_here("unsupported type definition (only enum/range/array/record-skip)");
            self.skip_to_semicolon();
        }
        self.expect_punct(';', "to end the type declaration");
        self.builder.finish_node();
    }

    /// VHDL-2019 type instantiation: `type x is new g generic map (...);` —
    /// parsed loosely (name + optional generic map).
    pub(super) fn parse_type_instantiation(&mut self) {
        self.bump();
        self.parse_name();
        if self.at_kw("generic") {
            self.bump();
            self.expect_kw("map", "after generic");
            self.parse_assoc_list();
        }
    }

    /// Enumeration type: `(idle, running, ...)` — literals are identifiers
    /// or character literals.
    pub(super) fn parse_enum_type(&mut self) {
        self.bump();
        loop {
            if self.peek().kind == TokenKind::Ident || self.peek().kind == TokenKind::CharLiteral {
                self.bump();
            } else {
                self.error_here("expected an enumeration literal");
                break;
            }
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct(')', "to close the enumeration");
    }

    /// `array (range) of type_mark [constraint | range ..]` — parsed loosely.
    pub(super) fn parse_array_type(&mut self) {
        self.bump();
        if self.at_punct('(') {
            self.parse_constraint();
        }
        self.expect_kw("of", "in the array type");
        self.parse_type_mark();
        if self.at_punct('(') {
            // Constrained element type: `... of std_logic_vector(31 downto 0)`.
            self.parse_constraint();
        } else if self.at_kw("range") {
            // Range-constrained element type: `... of integer range 3 downto 0`.
            self.start(SyntaxKind::Range);
            self.bump();
            self.parse_range();
            self.builder.finish_node();
        }
    }

    /// `record field {, field} : type [constraint] ; ... end record;`
    pub(super) fn parse_record_type(&mut self) {
        self.bump();
        while !self.at_kw("end") && self.peek().kind != TokenKind::Eof {
            self.parse_record_field();
        }
        self.expect_kw("end", "to close the record type");
        self.eat_kw("record");
        if self.peek().kind == TokenKind::Ident {
            self.bump(); // optional record name
        }
    }

    /// One record element: `field {, field} : type_mark [constraint] ;`
    pub(super) fn parse_record_field(&mut self) {
        loop {
            if self.peek().kind == TokenKind::Ident {
                self.parse_name();
            }
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct(':', "before the record field type");
        self.parse_type_mark();
        if self.at_punct('(') {
            self.parse_constraint();
        } else if self.at_kw("range") {
            self.start(SyntaxKind::Range);
            self.bump();
            self.parse_range();
            self.builder.finish_node();
        }
        self.expect_punct(';', "to end the record field");
    }

    /// `protected ... end protected [name];` and the separate-body form
    /// `type X is protected body ... end protected body [X];`
    pub(super) fn parse_protected_type(&mut self) {
        self.bump();
        if self.at_kw("body") {
            self.bump();
        }
        while !self.at_kw("end") && self.peek().kind != TokenKind::Eof {
            self.parse_decl();
        }
        self.expect_kw("end", "to close the protected type");
        self.eat_kw("protected");
        // `end protected [body] [name];`
        self.eat_kw("body");
        if self.peek().kind == TokenKind::Ident {
            self.bump(); // optional type name
        }
    }

    /// `units base; name = value unit; ... end units;` — physical type body.
    pub(super) fn parse_units(&mut self) {
        self.bump(); // units
        loop {
            if self.at_kw("end") || self.peek().kind == TokenKind::Eof {
                break;
            }
            if self.peek().kind == TokenKind::Ident {
                self.parse_name(); // base unit or derived unit name
                if self.eat_punct('=') {
                    self.parse_expression();
                    if self.peek().kind == TokenKind::Ident {
                        self.bump(); // unit name
                    }
                }
                self.expect_punct(';', "in the units declaration");
            } else {
                self.bump();
            }
        }
        self.expect_kw("end", "to close the units declaration");
        self.eat_kw("units");
        if self.peek().kind == TokenKind::Ident {
            self.bump(); // optional unit name: `end units length;`
        }
    }

    pub(super) fn parse_subtype_decl(&mut self) {
        self.start(SyntaxKind::SubtypeDecl);
        self.bump(); // subtype
        self.expect_ident("for the subtype name");
        self.expect_kw("is", "after the subtype name");
        // Optional resolution function: `subtype s is resolved std_logic;`,
        // `subtype s is my_resolver std_logic;`, or the parenthesized form
        // `subtype s is (my_resolver) std_logic;`.
        if self.at_kw("resolved") {
            self.bump();
        } else if self.at_punct('(') {
            self.bump();
            if self.peek().kind == TokenKind::Ident {
                self.bump();
            }
            self.expect_punct(')', "to close the resolution function");
        } else if self.peek().kind == TokenKind::Ident
            && self.peek2().kind == TokenKind::Ident
            && !self.peek2_text().eq_ignore_ascii_case("range")
        {
            self.bump(); // resolution function name
        }
        self.parse_type_mark();
        // Type attribute in the indication: `subtype E is V'element;` /
        // `subtype I is V'index;` (VHDL-2019).
        if self.at_punct('\'') {
            self.bump();
            if self.peek().kind == TokenKind::Ident {
                self.bump(); // attribute name
            } else {
                self.error_here("expected an attribute name after the tick");
            }
        }
        while self.at_punct('(') {
            self.parse_constraint();
        }
        if self.at_kw("range") {
            self.bump();
            self.parse_range();
        }
        self.expect_punct(';', "to end the subtype declaration");
        self.builder.finish_node();
    }

    /// A type mark: a (possibly selected) name, e.g. `std_logic_vector`.
    /// No paren suffixes — a following `(7 downto 0)` is a constraint, parsed
    /// separately by the caller.
    pub(super) fn parse_type_mark(&mut self) {
        if self.peek().kind == TokenKind::Ident {
            self.start(SyntaxKind::Name);
            self.bump();
            while self.at_punct('.') {
                self.bump();
                if self.peek().kind == TokenKind::Ident {
                    self.bump();
                } else {
                    self.error_here("expected a name after '.'");
                    break;
                }
            }
            self.builder.finish_node();
            // Subtype attribute as a type mark: `A'subtype` — keep the base
            // name as the type mark, consume the tick + attribute name.
            if self.at_punct('\'') && self.peek2().kind == TokenKind::Ident {
                self.bump();
                self.bump();
            }
        } else {
            self.error_here("expected a type mark");
            self.bump();
        }
    }

    /// `(expr to|downto expr)` / `(expr)` / `range ...` — an index/range constraint.
    pub(super) fn parse_constraint(&mut self) {
        // `(r)` or multi-dimensional `(r1, r2, ...)` — each sub-range is its
        // own Range child of the outer constraint node.
        self.start(SyntaxKind::Range);
        self.expect_punct('(', "to open the constraint");
        loop {
            self.start(SyntaxKind::Range);
            self.parse_range();
            self.builder.finish_node();
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct(')', "to close the constraint");
        self.builder.finish_node();
    }

    pub(super) fn parse_range(&mut self) {
        // Box constraint `range <>` (unconstrained index) — the `range`
        // keyword may come first (`range <>`) or after an index type mark
        // (`natural range <>`), so check both positions.
        if self.at_kw("range") {
            self.bump();
        }
        // Box `<>` may also follow a `range` keyword already consumed by the
        // caller (type/subtype declarations bump `range` themselves).
        if self.peek_text() == "<" && self.peek2_text() == ">" {
            self.bump();
            self.bump(); // <>
            return;
        }
        self.parse_expression();
        if self.at_kw("to") || self.at_kw("downto") {
            self.bump();
            self.parse_expression();
        } else if self.at_kw("range") {
            self.bump();
            if self.peek_text() == "<" && self.peek2_text() == ">" {
                self.bump();
                self.bump(); // <>
            } else {
                // `subtype_name range lo to hi` / `... range lo downto hi`.
                self.parse_expression();
                if self.at_kw("to") || self.at_kw("downto") {
                    self.bump();
                    self.parse_expression();
                }
            }
        }
    }
}
