//! Process items and event controls: `always` (any flavor, with its
//! `@`-sensitivity), `initial`, `final`, `event` declarations, and the
//! `@(...) / @* / @sig` sensitivity-list grammar shared by `always` and
//! event waits (including SV `iff` qualifiers). Child module of
//! `verilog::parser`, the same pattern as `sva.rs`.

use crate::kinds::SyntaxKind;
use crate::token::TokenKind;

impl super::Parser<'_> {
    pub(super) fn parse_always(&mut self) {
        self.builder.start_node(SyntaxKind::Always);
        self.bump(); // always*
        if self.eat_punct('@') {
            self.parse_sens_list();
        }
        self.parse_statement();
        self.builder.finish_node();
    }

    /// `@(...)`, `@*`, or `@sig` — shared by `always` and event waits.
    pub(super) fn parse_sens_list(&mut self) {
        if self.at_punct('(') {
            self.builder.start_node(SyntaxKind::SensList);
            self.bump();
            // Items: `[posedge|negedge] expr [iff cond]` separated by `,` or
            // `or`; `@(*)` is all.
            while !self.at_punct(')') && self.peek().kind != TokenKind::Eof {
                if self.at_punct('*') {
                    self.bump();
                    break;
                }
                if self.at_kw("posedge") || self.at_kw("negedge") {
                    self.bump();
                }
                self.parse_expression();
                if self.at_kw("iff") {
                    // Event qualifier (SV LRM 9.4.2.3): fires the preceding
                    // edge only when the condition is true at that edge.
                    self.builder.start_node(SyntaxKind::IffCond);
                    self.bump();
                    self.parse_expression();
                    self.builder.finish_node();
                }
                if !self.eat_punct(',') && !self.eat_kw("or") {
                    break;
                }
            }
            self.expect_punct(')', "to close the sensitivity list");
            self.builder.finish_node();
        } else if self.at_punct('*') {
            self.bump();
        } else {
            // `@sig` — single signal.
            self.parse_expression();
        }
    }

    pub(super) fn parse_initial(&mut self) {
        self.builder.start_node(SyntaxKind::Initial);
        self.bump(); // initial
        self.parse_statement();
        self.builder.finish_node();
    }

    /// `event a, b;`
    pub(super) fn parse_event_decl(&mut self) {
        self.builder.start_node(SyntaxKind::EventDecl);
        self.bump(); // event
        loop {
            if self.peek().kind == TokenKind::Ident {
                self.bump_name_node();
            } else {
                self.error_here("expected an event name");
                break;
            }
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct(';', "to end the event declaration");
        self.builder.finish_node();
    }

    /// `final` block — like `initial`, runs at the end of simulation.
    pub(super) fn parse_final(&mut self) {
        self.builder.start_node(SyntaxKind::Final);
        self.bump(); // final
        self.parse_statement();
        self.builder.finish_node();
    }
}
