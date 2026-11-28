//! Verilog generate constructs: `generate` regions, implicit generate
//! items at module level (`if`/`for`/`case`/`begin`), named generate blocks,
//! and `genvar` for-loops. Item bodies recurse into module items, so
//! generate and module-body parsing are mutually recursive. Child module of
//! `verilog::parser` (house pattern: cohesive grammar slices live in child
//! modules to keep the parser file at its standards baseline).

use crate::kinds::SyntaxKind;
use crate::token::TokenKind;

impl super::Parser<'_> {
    pub(super) fn parse_generate(&mut self) {
        self.builder.start_node(SyntaxKind::Generate);
        self.bump(); // generate
        while !self.at_kw("endgenerate")
            && !self.at_kw("endmodule")
            && self.peek().kind != TokenKind::Eof
        {
            self.parse_gen_item();
        }
        if !self.eat_kw("endgenerate") {
            self.error_here("expected `endgenerate`");
        }
        self.builder.finish_node();
    }

    /// One generate-region item: a begin-block, nested for/if/case, or a
    /// module item. Also used for implicit generate regions at module level
    /// (`if (...) begin ... end` without the `generate` keyword).
    pub(super) fn parse_gen_item(&mut self) {
        if self.at_kw("begin") {
            self.parse_gen_block();
        } else if self.at_kw("for") {
            self.parse_gen_for();
        } else if self.at_kw("if") {
            self.parse_gen_if();
        } else if self.at_kw("case") || self.at_kw("casez") || self.at_kw("casex") {
            self.parse_gen_case();
        } else {
            self.parse_module_item();
        }
    }

    /// `begin : name module-items end` inside a generate region.
    fn parse_gen_block(&mut self) {
        self.builder.start_node(SyntaxKind::GenBlock);
        self.bump(); // begin
        if self.eat_punct(':') && self.peek().kind == TokenKind::Ident {
            self.builder.start_node(SyntaxKind::Name);
            self.bump();
            self.builder.finish_node();
        }
        while !self.at_kw("end")
            && !self.at_kw("endgenerate")
            && !self.at_kw("endmodule")
            && self.peek().kind != TokenKind::Eof
        {
            self.parse_gen_item();
        }
        if !self.eat_kw("end") {
            self.error_here("expected `end`");
        }
        self.builder.finish_node();
    }

    /// `case (expr) item: items... endcase` in a generate region. Item bodies
    /// are module items; a new item starts at an expression or `default`.
    fn parse_gen_case(&mut self) {
        self.builder.start_node(SyntaxKind::GenCase);
        self.bump(); // case/casez/casex
        self.expect_punct('(', "after `case`");
        self.parse_expression();
        self.expect_punct(')', "to close the case expression");
        loop {
            if self.at_kw("endcase") || self.peek().kind == TokenKind::Eof {
                break;
            }
            self.builder.start_node(SyntaxKind::CaseItem);
            if self.at_kw("default") {
                self.bump();
            } else {
                loop {
                    self.parse_expression();
                    if !self.eat_punct(',') {
                        break;
                    }
                }
            }
            self.expect_punct(':', "before the generate case item body");
            while !self.at_kw("endcase") && !self.at_gen_case_item_start() {
                if self.peek().kind == TokenKind::Eof {
                    break;
                }
                if self.at_kw("for") {
                    self.parse_gen_for();
                } else if self.at_kw("if") {
                    self.parse_gen_if();
                } else if self.at_kw("case") || self.at_kw("casez") || self.at_kw("casex") {
                    self.parse_gen_case();
                } else if self.at_kw("begin") {
                    self.parse_gen_block();
                } else {
                    self.parse_module_item();
                }
            }
            self.builder.finish_node();
        }
        if !self.eat_kw("endcase") {
            self.error_here("expected `endcase`");
        }
        self.builder.finish_node();
    }

    /// Heuristic: does the current position start a new generate-case item
    /// (an expression label or `default`) rather than a module item?
    fn at_gen_case_item_start(&self) -> bool {
        if self.at_kw("default") {
            return true;
        }
        // `begin` opens a named generate block in an item body, not a label.
        if self.at_kw("begin") {
            return false;
        }
        match self.peek().kind {
            TokenKind::IntLiteral | TokenKind::RealLiteral | TokenKind::StrLiteral => true,
            TokenKind::Ident => {
                let t2 = self.peek2_text();
                matches!(t2.as_str(), ":" | ",")
            }
            _ => false,
        }
    }

    fn parse_gen_for(&mut self) {
        self.builder.start_node(SyntaxKind::GenFor);
        self.bump(); // for
        self.expect_punct('(', "after `for`");
        self.eat_kw("genvar"); // `for (genvar j = 0; ...)`
        self.parse_assign_no_semi(); // init
        self.expect_punct(';', "in for header");
        self.parse_expression(); // condition
        self.expect_punct(';', "in for header");
        self.parse_assign_no_semi(); // step
        self.expect_punct(')', "to close the for header");
        self.parse_gen_item();
        self.builder.finish_node();
    }

    fn parse_gen_if(&mut self) {
        self.builder.start_node(SyntaxKind::GenIf);
        self.bump(); // if
        self.expect_punct('(', "after `if`");
        self.parse_expression();
        self.expect_punct(')', "to close the if condition");
        self.parse_gen_item();
        if self.eat_kw("else") {
            self.parse_gen_item();
        }
        self.builder.finish_node();
    }
}
