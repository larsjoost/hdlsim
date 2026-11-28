//! Interface declarations: the formal lists of generic/port/parameter
//! clauses — `name [, ...] : [class] [mode] type_mark [constraint]
//! [:= default]`, VHDL-2008 type generics (`type T;`), VHDL-2019
//! `type T is <subtype indication>` formals and view modes.
//!
//! Child module of `vhdl::parser`, the same pattern as `decl.rs`.

use crate::kinds::SyntaxKind;
use crate::token::TokenKind;

impl super::Parser<'_> {
    /// Is the current token an interface class/mode keyword?
    pub(super) fn at_class_or_mode_kw(&self) -> bool {
        const KWS: &[&str] = &[
            "in", "out", "inout", "buffer", "linkage", "signal", "constant", "variable",
        ];
        self.peek().kind == TokenKind::Ident
            && KWS.contains(&self.peek_text().to_ascii_lowercase().as_str())
    }

    /// Parameter class keyword (signal/constant/variable/file/type) — precedes the names.
    pub(super) fn at_param_class_kw(&self) -> bool {
        const KWS: &[&str] = &["signal", "constant", "variable", "file", "type"];
        self.peek().kind == TokenKind::Ident
            && KWS.contains(&self.peek_text().to_ascii_lowercase().as_str())
    }

    /// Skip a subtype indication inside an interface list, stopping at the
    /// `;` or the `)` closing the list (whichever comes first at bracket
    /// depth zero) — for `type T is <subtype indication>` formals.
    pub(super) fn skip_to_interface_end(&mut self) {
        let mut depth = 0;
        while self.peek().kind != TokenKind::Eof {
            if self.at_punct('(') || self.at_punct('[') {
                depth += 1;
            } else if self.at_punct(')') || self.at_punct(']') {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            } else if depth == 0 && self.at_punct(';') {
                break;
            }
            self.bump();
        }
    }

    /// `name [, name...] : [mode] type_mark [constraint] [:= default]`.
    pub(super) fn parse_interface_decl(&mut self, kind: SyntaxKind) {
        self.start(kind);
        // Optional parameter class BEFORE the names: `signal s : t`,
        // `constant c : t`, `variable v : t` (subprogram parameters),
        // `type T` (VHDL-2008 type generic in a generic clause).
        let class = if self.at_param_class_kw() {
            let t = self.peek_text().to_ascii_lowercase();
            self.bump();
            Some(t)
        } else {
            None
        };
        self.parse_interface_names();
        // `type T;` — a type generic has no mode or type mark.
        // VHDL-2019: `type T is <subtype indication>` (e.g.
        // `type V is array (type is range <>) of type is private`).
        if class.as_deref() == Some("type") && !self.at_punct(':') {
            self.parse_interface_type_generic();
            self.builder.finish_node();
            return;
        }
        self.expect_punct(':', "before the mode/type");
        self.parse_interface_mode_and_type();
        self.parse_interface_tail();
        self.builder.finish_node();
    }

    /// The comma-separated formal names; a class/mode keyword ends the list.
    pub(super) fn parse_interface_names(&mut self) {
        loop {
            if self.peek().kind == TokenKind::Ident && !self.at_class_or_mode_kw() {
                self.parse_name();
            }
            if !self.eat_punct(',') {
                break;
            }
        }
    }

    /// A type generic formal without a mode/type mark: bare `type T` or the
    /// VHDL-2019 `type T is <subtype indication>`. The subtype indication is
    /// skipped with bracket/paren tracking so the closing `)` of the generic
    /// list stays for the caller.
    pub(super) fn parse_interface_type_generic(&mut self) {
        if self.at_kw("is") {
            self.bump();
        }
        self.skip_to_interface_end();
    }

    /// What follows the `:` — the VHDL-2019 anonymous-type formal
    /// (`A : type is private;` / `B : type is <>;`, where the `type` keyword
    /// with its optional subtype indication acts as the type mark), a view
    /// mode, an explicit mode, or just the type mark.
    pub(super) fn parse_interface_mode_and_type(&mut self) {
        if self.at_kw("type") {
            self.bump();
            if self.eat_kw("is") {
                self.skip_to_interface_end();
            }
        } else if self.at_kw("view") {
            self.bump();
            if self.eat_punct('(') {
                self.parse_name(); // view name
                self.expect_punct(')', "to close the parenthesized view name");
            } else {
                self.parse_name(); // view name
            }
            self.expect_kw("of", "in the view mode");
            self.parse_type_mark();
        } else if self.at_kw("in")
            || self.at_kw("out")
            || self.at_kw("inout")
            || self.at_kw("buffer")
            || self.at_kw("linkage")
        {
            // Optional mode after the colon: in/out/inout/buffer/linkage.
            self.bump();
            self.parse_type_mark();
        } else {
            self.parse_type_mark();
        }
    }

    /// Constraints, the port kind (`bus`/`register`), an unparenthesized
    /// range constraint, and the optional `:=` default expression.
    pub(super) fn parse_interface_tail(&mut self) {
        while self.at_punct('(') {
            self.parse_constraint();
        }
        // Port kind: `signal q : out resolved_byte bus)`.
        if self.at_kw("bus") || self.at_kw("register") {
            self.bump();
        }
        if self.at_kw("range") {
            // `natural range 1 to 3` — range constraint without parens.
            self.start(SyntaxKind::Range);
            self.bump();
            self.parse_range();
            self.builder.finish_node();
        }
        if self.peek_op2().as_deref() == Some(":=") {
            self.bump();
            self.bump();
            self.parse_expression();
        }
    }
}
