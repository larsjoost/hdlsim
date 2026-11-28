//! SVA-lite concurrent property parsing: `assert|assume|cover property` and
//! the `|->` / `|=>` property operators. Child module of `verilog::parser`
//! (house pattern: cohesive grammar slices live in child modules to keep the
//! parser file at its standards baseline). Parsed but not simulated — the
//! lowerer turns assert-properties into checked always-processes.

use crate::kinds::SyntaxKind;

impl super::Parser<'_> {
    /// `assert|assume|cover property (@(sens) prop);` module item (SVA-lite).
    pub(super) fn parse_sva_property(&mut self, kw: &str) {
        let kind = match kw {
            "assert" => SyntaxKind::AssertProperty,
            "assume" => SyntaxKind::AssumeProperty,
            _ => SyntaxKind::CoverProperty,
        };
        self.builder.start_node(kind);
        self.bump(); // assert/assume/cover
        if !self.eat_kw("property") {
            self.error_here("expected `property`");
        }
        if !self.eat_punct('(') {
            self.error_here("expected `(` after `property`");
        }
        if self.eat_punct('@') {
            self.parse_sens_list();
        } else {
            self.error_here("SVA-lite property needs a clocking event, e.g. `@(posedge clk)`");
        }
        self.parse_property();
        self.expect_punct(')', "to close the property");
        self.expect_punct(';', "to end the property declaration");
        self.builder.finish_node();
    }

    /// Property expression: `expr`, `expr |-> expr`, or `expr |=> expr`.
    /// `|->` / `|=>` lex as the single-char puncts `|`, `-`/`=`, `>`, so the
    /// antecedent/consequent are parsed one precedence level above `|`.
    pub(super) fn parse_property(&mut self) {
        self.builder.start_node(SyntaxKind::Property);
        let mark = self.builder.mark();
        self.parse_unary();
        self.parse_binops(mark, 7);
        let imp = if self.at_punct('|') && self.peek2_text() == "-" && self.peek3_text() == ">" {
            Some("|->")
        } else if self.at_punct('|') && self.peek2_text() == "=" && self.peek3_text() == ">" {
            Some("|=>")
        } else {
            None
        };
        if imp.is_some() {
            self.bump(); // |
            self.bump(); // - or =
            self.bump(); // >
            let mark2 = self.builder.mark();
            self.parse_unary();
            self.parse_binops(mark2, 7);
        } else {
            // Plain boolean property: allow the remaining top-level `|`.
            self.parse_binops(mark, 6);
        }
        self.builder.finish_node();
    }
}
