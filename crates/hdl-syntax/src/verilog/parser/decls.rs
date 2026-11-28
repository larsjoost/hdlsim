//! Verilog declarations and assignments: non-ANSI port declarations, net/
//! variable declarations (with SV modifiers and unpacked dimensions),
//! parameter/localparam declarations, bare enum declarations, continuous
//! `assign` (with drive strength and `#delay`), the assignment operators
//! (`=`, `<=`, compound), functions and tasks, and `defparam`. Child module
//! of `verilog::parser` (house pattern: cohesive grammar slices live in
//! child modules to keep the parser file at its standards baseline).

use super::DECL_KEYWORDS;
use crate::kinds::SyntaxKind;
use crate::token::TokenKind;

impl super::Parser<'_> {
    /// `enum [base] { A, B = 2, ... } name [= init] [, name2 ...];` — a bare
    /// enum type declaration with variable declarators in a module body.
    pub(super) fn parse_enum_decl(&mut self) {
        self.builder.start_node(SyntaxKind::Decl);
        self.parse_enum_type();
        loop {
            if self.peek().kind == TokenKind::Ident {
                self.bump_name_node();
            } else {
                self.error_here("expected a name in the enum declaration");
                break;
            }
            self.parse_optional_range();
            if self.eat_punct('=') {
                self.parse_expression();
            }
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct(';', "to end the enum declaration");
        self.builder.finish_node();
    }

    /// Non-ANSI port declaration inside the module body: `input [7:0] a, b;`
    pub(super) fn parse_port_decl_stmt(&mut self) {
        self.builder.start_node(SyntaxKind::Port);
        self.bump(); // direction
        self.eat_type_words(); // `input logic`, `input int unsigned`, ...
                               // Optional user type name: `input data_t d_i;`.
        if self.peek().kind == TokenKind::Ident
            && (self.peek2().kind == TokenKind::Ident || self.peek2_text() == "[")
        {
            self.bump();
        }
        self.parse_optional_range();
        loop {
            if self.peek().kind == TokenKind::Ident {
                self.bump_name_node();
            } else {
                self.error_here("expected port name");
                break;
            }
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct(';', "to end the port declaration");
        self.builder.finish_node();
    }

    /// `wire [7:0] a = 0, b;` etc. SV modifiers (`var`/`const`/`rand`/`randc`,
    /// `vectored`/`scalared`) may precede the type keyword.
    pub(super) fn parse_decl(&mut self) {
        self.builder.start_node(SyntaxKind::Decl);
        while self.at_kw("var")
            || self.at_kw("const")
            || self.at_kw("rand")
            || self.at_kw("randc")
            || self.at_kw("vectored")
            || self.at_kw("scalared")
        {
            self.bump();
        }
        self.bump(); // net/reg type keyword
        while self.at_kw("signed") || self.at_kw("unsigned") {
            self.bump();
        }
        while self.at_punct('[') {
            self.parse_optional_range(); // multi-dimensional: `logic [A][B] x;`
        }
        loop {
            if self.peek().kind == TokenKind::Ident {
                self.bump_name_node();
            } else {
                self.error_here("expected a name in declaration");
                break;
            }
            while self.at_punct('[') {
                self.parse_optional_range(); // per-name unpacked dims: `x [3:0]`
            }
            if self.eat_punct('=') {
                self.parse_expression();
            }
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct(';', "to end the declaration");
        self.builder.finish_node();
    }

    pub(super) fn parse_param_decl(&mut self) {
        self.builder.start_node(SyntaxKind::ParamDecl);
        self.bump(); // parameter/localparam
        if self.at_kw("type") {
            self.bump();
            if self.peek().kind == TokenKind::Ident {
                // `parameter type data_t = logic` declares a type param.
                self.user_types.insert(self.peek_text().to_string());
            }
        }
        self.eat_type_words(); // `parameter int unsigned W`, ...
                               // Optional user type name: `localparam cb_seed_t [2:0] X = ...`.
        if self.peek().kind == TokenKind::Ident
            && (self.peek2().kind == TokenKind::Ident || self.peek2_text() == "[")
        {
            self.bump();
        }
        // Optional range: `parameter [7:0] W = 8;`.
        self.parse_optional_range();
        loop {
            if self.peek().kind == TokenKind::Ident {
                self.bump_name_node();
            } else {
                self.error_here("expected parameter name");
                break;
            }
            self.expect_punct('=', "in parameter declaration");
            self.parse_expression();
            self.parse_optional_range(); // `parameter type T = logic [7:0]`
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct(';', "to end the parameter declaration");
        self.builder.finish_node();
    }

    /// The value after a `#` inside the open Delay node: a `(...)` comma
    /// list or a single primary.
    pub(super) fn parse_delay_value(&mut self, close_msg: &str) {
        if self.eat_punct('(') {
            loop {
                self.parse_expression();
                if !self.eat_punct(',') {
                    break;
                }
            }
            self.expect_punct(')', close_msg);
        } else {
            self.parse_primary();
        }
    }

    /// `assign [#delay] [(strengths)] lhs = [#delay] rhs;`
    pub(super) fn parse_assign(&mut self) {
        self.builder.start_node(SyntaxKind::Assign);
        self.bump(); // `assign`
        if self.at_punct('#') {
            // `assign #5 a = b;` / `assign #(1,2,3) a = b;`
            self.builder.start_node(SyntaxKind::Delay);
            self.bump();
            self.parse_delay_value("to close the assign delay");
            self.builder.finish_node();
        }
        if self.at_punct('(') && self.peek2().kind == TokenKind::Ident && self.is_strength(1) {
            // Drive strength list: `(strong1, pull0)`.
            self.bump();
            loop {
                self.bump(); // strength name
                if !self.eat_punct(',') {
                    break;
                }
            }
            self.expect_punct(')', "to close the strength list");
        }
        self.parse_expression(); // lhs
        self.expect_punct('=', "in continuous assignment");
        if self.at_punct('#') {
            self.builder.start_node(SyntaxKind::Delay);
            self.bump();
            self.parse_primary();
            self.builder.finish_node();
        }
        self.parse_expression(); // rhs
        self.expect_punct(';', "to end the assignment");
        self.builder.finish_node();
    }

    /// Is the `ahead`-th non-trivia token one of the drive-strength keywords?
    pub(super) fn is_strength(&self, ahead: usize) -> bool {
        let mut i = self.skip_trivia_from(self.pos);
        for _ in 0..ahead {
            i = self.skip_trivia_from(i + 1);
        }
        if i >= self.tokens.len() {
            return false;
        }
        let t = &self.tokens[i];
        if t.kind != TokenKind::Ident {
            return false;
        }
        let text = &self.src.text()[t.span.start as usize..t.span.end as usize];
        matches!(
            text,
            "strong0" | "strong1" | "pull0" | "pull1" | "weak0" | "weak1" | "highz0" | "highz1"
        )
    }

    /// Consume `=` (blocking) or `<=` (non-blocking). Returns false if neither.
    pub(super) fn parse_assign_op(&mut self) -> bool {
        // Two/three-char operators lex as separate puncts; match on raw text.
        let t2 = format!("{}{}", self.peek_text(), self.peek2_text());
        let t3 = format!(
            "{}{}{}",
            self.peek_text(),
            self.peek2_text(),
            self.peek3_text()
        );
        for op in ["<<<=", ">>>="] {
            if t3 == op {
                self.bump();
                self.bump();
                self.bump();
                return true;
            }
        }
        for op in ["<<=", ">>=", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^="] {
            if t2 == op {
                self.bump();
                self.bump();
                return true;
            }
        }
        if self.peek_op2().as_deref() == Some("<=") {
            self.bump();
            self.bump();
            true
        } else if self.at_punct('=') {
            self.bump();
            true
        } else {
            false
        }
    }

    /// `lhs = expr` without a trailing semicolon (for-loop init/step).
    pub(super) fn parse_assign_no_semi(&mut self) {
        self.builder.start_node(SyntaxKind::StmtAssign);
        self.eat_type_words(); // `for (int i = 0; ...)`
                               // `for (...; i++)` step: a bare inc/dec needs no assignment operator.
        let is_incdec = (self.peek2_text() == "+" && self.peek3_text() == "+")
            || (self.peek2_text() == "-" && self.peek3_text() == "-");
        self.parse_postfix();
        if !self.parse_assign_op() {
            if is_incdec {
                self.builder.finish_node();
                return;
            }
            self.error_here("expected `=` or `<=` in for-loop assignment");
        }
        self.parse_expression();
        self.builder.finish_node();
    }

    /// `function [automatic] [void|type] [range] name [(ports)] ; body endfunction`.
    /// Ports may be ANSI-style `(input [7:0] a)` or plain `input a;` decls.
    pub(super) fn parse_function(&mut self) {
        self.builder.start_node(SyntaxKind::Function);
        self.bump(); // function
        self.eat_kw("automatic");
        if self.at_kw("void") {
            self.bump();
        } else {
            self.eat_type_words(); // `longint unsigned`, `logic`, ...
        }
        self.parse_optional_range();
        if self.peek().kind == TokenKind::Ident {
            self.builder.start_node(SyntaxKind::Name);
            self.bump();
            self.builder.finish_node();
        } else {
            self.error_here("expected function name");
        }
        if self.eat_punct('(') {
            // ANSI function port list.
            while !self.at_punct(')') && self.peek().kind != TokenKind::Eof {
                self.parse_port();
                if !self.eat_punct(',') {
                    break;
                }
            }
            self.expect_punct(')', "to close the function port list");
        }
        self.expect_punct(';', "after the function header");
        while !self.at_kw("endfunction") && self.peek().kind != TokenKind::Eof {
            if self.at_kw("input") || self.at_kw("output") || self.at_kw("inout") {
                self.parse_port_decl_stmt();
            } else if self.at_kw("parameter") || self.at_kw("localparam") {
                self.parse_param_decl();
            } else {
                self.parse_decl_or_stmt();
            }
        }
        if !self.eat_kw("endfunction") {
            self.error_here("expected `endfunction`");
        }
        self.builder.finish_node();
    }

    /// `task [automatic] name [(ports)]; decls/stmts endtask`.
    pub(super) fn parse_task(&mut self) {
        self.builder.start_node(SyntaxKind::Task);
        self.bump(); // task
        self.eat_kw("automatic");
        if self.peek().kind == TokenKind::Ident {
            self.builder.start_node(SyntaxKind::Name);
            self.bump();
            self.builder.finish_node();
        } else {
            self.error_here("expected task name");
        }
        if self.eat_punct('(') {
            while !self.at_punct(')') && self.peek().kind != TokenKind::Eof {
                self.parse_port();
                if !self.eat_punct(',') {
                    break;
                }
            }
            self.expect_punct(')', "to close the task port list");
        }
        self.expect_punct(';', "after the task header");
        while !self.at_kw("endtask") && self.peek().kind != TokenKind::Eof {
            if self.at_kw("input") || self.at_kw("output") || self.at_kw("inout") {
                self.parse_port_decl_stmt();
            } else {
                self.parse_decl_or_stmt();
            }
        }
        if !self.eat_kw("endtask") {
            self.error_here("expected `endtask`");
        }
        self.builder.finish_node();
    }

    /// Decl keywords inside a task/function body vs. plain statements.
    fn parse_decl_or_stmt(&mut self) {
        let text = self.peek_text().to_string();
        match text.as_str() {
            _ if DECL_KEYWORDS.contains(&text.as_str()) => self.parse_decl(),
            "parameter" | "localparam" => self.parse_param_decl(),
            "import" => self.parse_import(false),
            "export" => self.parse_import(true),
            "automatic" | "static" => {
                self.bump();
                self.parse_decl_or_stmt();
            }
            _ if self.user_types.contains(&text) && self.peek2().kind == TokenKind::Ident => {
                self.parse_decl()
            }
            _ => self.parse_statement(),
        }
    }

    /// `defparam a.b.P = expr, ...;`
    pub(super) fn parse_defparam(&mut self) {
        self.builder.start_node(SyntaxKind::Defparam);
        self.bump(); // defparam
        loop {
            // Hierarchical parameter path.
            self.builder.start_node(SyntaxKind::Name);
            if self.peek().kind == TokenKind::Ident {
                self.bump();
                while self.at_punct('.') {
                    self.bump();
                    if self.peek().kind == TokenKind::Ident {
                        self.bump();
                    } else {
                        break;
                    }
                }
            }
            self.builder.finish_node();
            self.expect_punct('=', "in defparam");
            self.parse_expression();
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct(';', "to end the defparam statement");
        self.builder.finish_node();
    }
}
