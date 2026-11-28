//! Verilog class bodies: the `class ... endclass` design unit (with
//! `extends`, class item modifiers, functions/tasks/constraints), class-body
//! declarations, and `constraint` blocks (parsed structurally, lowered
//! later). Child module of `verilog::parser` (house pattern: cohesive
//! grammar slices live in child modules to keep the parser file at its
//! standards baseline).

use crate::kinds::SyntaxKind;
use crate::token::TokenKind;

impl super::Parser<'_> {
    /// `class [virtual] name [extends base]; items... endclass [name];`
    pub(super) fn parse_class(&mut self) {
        self.builder.start_node(SyntaxKind::Class);
        if self.at_kw("virtual") {
            self.bump();
        }
        self.bump(); // class
        let mut cname = String::new();
        if self.peek().kind == TokenKind::Ident {
            self.builder.start_node(SyntaxKind::Name);
            cname = self.peek_text().to_string();
            self.bump();
            self.builder.finish_node();
        } else {
            self.error_here("expected a class name");
        }
        if !cname.is_empty() {
            self.user_types.insert(cname.clone());
        }
        if self.at_kw("extends") {
            self.bump();
            if self.peek().kind == TokenKind::Ident {
                self.builder.start_node(SyntaxKind::Name);
                self.bump();
                self.builder.finish_node();
            }
        }
        self.expect_punct(';', "after the class header");
        while !self.at_kw("endclass") && self.peek().kind != TokenKind::Eof {
            self.parse_class_item();
        }
        self.eat_kw("endclass");
        self.eat_matching_trailing_name(&cname);
        self.eat_punct(';'); // optional
        self.builder.finish_node();
    }

    /// One class-body item: a subroutine, a constraint, a declaration with a
    /// known user type, a modifier keyword preceding another item, or a
    /// recovered error.
    fn parse_class_item(&mut self) {
        let text = self.peek_text().to_string();
        if text == "function" {
            self.parse_function();
        } else if text == "task" {
            self.parse_task();
        } else if text == "constraint" {
            self.parse_constraint();
        } else if self.user_types.contains(&text) && self.peek2().kind == TokenKind::Ident {
            self.parse_decl();
        } else if text == "static"
            || text == "protected"
            || text == "local"
            || text == "virtual"
            || text == "pure"
        {
            self.bump(); // class item modifier
        } else {
            self.parse_decl_or_stmt_class_item();
        }
    }

    /// Declaration or constraint inside a class body.
    fn parse_decl_or_stmt_class_item(&mut self) {
        let text = self.peek_text().to_string();
        match text.as_str() {
            "wire" | "reg" | "logic" | "integer" | "real" | "time" | "bit" | "byte"
            | "shortint" | "int" | "longint" | "string" | "chandle" | "var" | "const" | "rand"
            | "randc" | "vectored" | "scalared" => self.parse_decl(),
            "constraint" => self.parse_constraint(),
            _ => {
                self.error_here(format!("unexpected `{}` in class body", text));
                // Recover: skip to the next `;`.
                while self.peek().kind != TokenKind::Eof
                    && !self.at_punct(';')
                    && !self.at_kw("endclass")
                {
                    self.bump();
                }
                self.eat_punct(';');
            }
        }
    }

    /// `constraint name { expr; ... }` — parsed structurally, lowered later.
    fn parse_constraint(&mut self) {
        self.builder.start_node(SyntaxKind::Constraint);
        self.bump(); // constraint
        if self.peek().kind == TokenKind::Ident {
            self.builder.start_node(SyntaxKind::Name);
            self.bump();
            self.builder.finish_node();
        }
        self.expect_punct('{', "to open the constraint block");
        let mut depth = 1;
        while depth > 0 && self.peek().kind != TokenKind::Eof {
            if self.at_punct('{') {
                depth += 1;
                self.bump();
            } else if self.at_punct('}') {
                depth -= 1;
                self.bump();
            } else {
                self.parse_constraint_expr();
            }
        }
        self.builder.finish_node();
    }

    /// One constraint expression with its separators: `;`, `and`, and
    /// `solve ... before` pairs (`a && b` parses as expressions).
    fn parse_constraint_expr(&mut self) {
        self.parse_expression();
        while self.at_punct(';') || self.at_kw("and") || self.at_kw("solve") {
            if self.at_kw("and") || self.at_kw("solve") {
                self.bump();
                if self.at_kw("before") {
                    self.bump();
                }
                continue;
            }
            self.bump(); // ;
            if self.at_punct('}') || self.at_kw("endclass") {
                break;
            }
        }
    }
}
