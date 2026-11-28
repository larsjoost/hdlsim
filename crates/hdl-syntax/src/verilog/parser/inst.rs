//! Verilog instantiation grammar: module instantiations (with `#(...)`
//! parameter overrides and named/`.*`/positional port connections) and gate
//! primitive instantiations (with drive strengths). Child module of
//! `verilog::parser` (house pattern: cohesive grammar slices live in child
//! modules to keep the parser file at its standards baseline).

use crate::kinds::SyntaxKind;
use crate::token::TokenKind;

impl super::Parser<'_> {
    /// `modname [#(...)] inst [range] (, ...) (port conns);`.
    pub(super) fn parse_instantiation(&mut self) {
        self.builder.start_node(SyntaxKind::Inst);
        // bump_name_node flushes leading trivia first — a comment directly
        // above must not join the module name, which is the design-unit name
        // resolved at elaboration.
        self.bump_name_node();
        // Optional parameter override `#(...)` — structured as ParamAssign nodes.
        if self.at_punct('#') {
            self.parse_inst_param_overrides();
        }
        // Instance name(s).
        loop {
            if self.peek().kind == TokenKind::Ident {
                self.bump_name_node();
            } else {
                self.error_here("expected instance name");
                break;
            }
            self.parse_optional_range();
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct('(', "before the port connections");
        self.parse_port_conn_list();
        self.expect_punct(')', "to close the port connections");
        self.expect_punct(';', "to end the instantiation");
        self.builder.finish_node();
    }

    /// `#( .P(expr), expr, ... )` — one ParamAssign per override, named
    /// (`.name(expr)`) or positional.
    fn parse_inst_param_overrides(&mut self) {
        self.bump(); // #
        self.expect_punct('(', "after `#`");
        while !self.at_punct(')') && self.peek().kind != TokenKind::Eof {
            self.builder.start_node(SyntaxKind::ParamAssign);
            if self.eat_punct('.') {
                if self.peek().kind == TokenKind::Ident {
                    self.builder.start_node(SyntaxKind::Name);
                    self.bump();
                    self.builder.finish_node();
                }
                self.expect_punct('(', "after the parameter name");
                if !self.at_punct(')') {
                    self.parse_expression();
                }
                self.expect_punct(')', "to close the parameter override");
            } else {
                // Positional override.
                self.parse_expression();
            }
            self.builder.finish_node();
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct(')', "to close the parameter override list");
    }

    /// Port connections between already-consumed parentheses: named
    /// `.port(expr)` / `.*`, or positional expressions, comma-separated.
    fn parse_port_conn_list(&mut self) {
        loop {
            if self.at_punct(')') || self.peek().kind == TokenKind::Eof {
                break;
            }
            self.parse_port_conn();
            if !self.eat_punct(',') {
                break;
            }
        }
    }

    fn parse_port_conn(&mut self) {
        self.builder.start_node(SyntaxKind::PortConn);
        if self.eat_punct('.') {
            if self.eat_punct('*') {
                // `.*`: unconnected ports bind to same-named signals; elab resolves.
                self.builder.finish_node();
                return;
            }
            self.parse_conn_name_and_value("to close the port connection");
        } else {
            // Positional connection.
            self.parse_expression();
        }
        self.builder.finish_node();
    }

    /// After the leading `.`, parse `name` with an optional `(expr)` — shared
    /// by port connections and named call arguments (same CST shape).
    pub(super) fn parse_conn_name_and_value(&mut self, close_msg: &str) {
        if self.peek().kind == TokenKind::Ident {
            self.builder.start_node(SyntaxKind::Name);
            self.bump();
            self.builder.finish_node();
        }
        if self.eat_punct('(') {
            if !self.at_punct(')') {
                self.parse_expression();
            }
            self.expect_punct(')', close_msg);
        }
    }

    /// Gate primitive instantiation: `and (o, a, b);`, `pullup (x);`,
    /// optionally with a drive-strength pair `(strong1, pull0)`.
    pub(super) fn parse_gate_inst(&mut self) {
        self.builder.start_node(SyntaxKind::Inst);
        self.builder.start_node(SyntaxKind::Name);
        self.bump(); // gate keyword
        self.builder.finish_node();
        if self.at_punct('(') && self.peek2().kind == TokenKind::Ident && self.is_strength(1) {
            self.bump();
            loop {
                self.bump();
                if !self.eat_punct(',') {
                    break;
                }
            }
            self.expect_punct(')', "to close the strength list");
        }
        // Optional instance name(s): `nand g1 [3:0] (n, a, b);`
        while self.peek().kind == TokenKind::Ident {
            self.builder.start_node(SyntaxKind::Name);
            self.bump();
            self.builder.finish_node();
            self.parse_optional_range();
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct('(', "before the gate ports");
        self.parse_port_conn_list();
        self.expect_punct(')', "to close the gate ports");
        self.expect_punct(';', "to end the gate instantiation");
        self.builder.finish_node();
    }
}
