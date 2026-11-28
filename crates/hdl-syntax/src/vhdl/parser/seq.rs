//! Sequential statements: the statement dispatch (labels), assert/report,
//! if/elsif/else, sequential block statements, case statements with their
//! alternatives, loops (for/while/infinite), wait, exit/next, return,
//! force/release (VHDL-2008), and the assignment/procedure-call forms
//! decided after the target name.
//!
//! Child module of `vhdl::parser`, the same pattern as `conc.rs`.

use crate::kinds::SyntaxKind;
use crate::token::TokenKind;

impl super::Parser<'_> {
    pub(super) fn parse_assert(&mut self, _concurrent: bool) {
        self.start(SyntaxKind::AssertStmt);
        self.bump(); // assert
        self.parse_expression(); // condition
        if self.eat_kw("report") {
            self.parse_expression();
        }
        if self.eat_kw("severity") && self.peek().kind == TokenKind::Ident {
            self.bump();
        }
        self.expect_punct(';', "to end the assert statement");
        self.builder.finish_node();
    }

    pub(super) fn parse_seq_stmt(&mut self) {
        if self.at_label() {
            self.bump(); // label
            self.bump(); // ':'
        }
        if self.at_kw("if") {
            self.parse_if();
        } else if self.at_kw("case") {
            self.parse_case();
        } else if self.at_kw("block") {
            // Sequential block statement: `lbl : block ... begin ... end block;`
            self.parse_block_stmt();
        } else if self.at_kw("for") {
            self.parse_for_loop();
        } else if self.at_kw("loop") || self.at_kw("while") {
            self.parse_loop();
        } else if self.at_kw("wait") {
            self.parse_wait();
        } else if self.at_kw("null") {
            self.start(SyntaxKind::NullStmt);
            self.bump();
            self.expect_punct(';', "after null");
            self.builder.finish_node();
        } else if self.at_kw("assert") {
            self.parse_assert(false);
        } else if self.at_kw("report") {
            self.parse_report_stmt();
        } else if self.at_kw("exit") || self.at_kw("next") {
            self.parse_exit_next();
        } else if self.at_kw("force") || self.at_kw("release") {
            self.parse_force_release();
        } else if self.at_kw("return") {
            self.parse_return_stmt();
        } else if self.at_punct('(') {
            // VHDL-2008 parenthesized target list: `(a, b) := f(...);`
            self.parse_seq_target_list();
        } else {
            self.parse_seq_assign_or_call();
        }
    }

    /// `report expr [severity expr];` — parsed as an AssertStmt node.
    pub(super) fn parse_report_stmt(&mut self) {
        self.start(SyntaxKind::AssertStmt);
        self.bump();
        self.parse_expression();
        if self.eat_kw("severity") && self.peek().kind == TokenKind::Ident {
            self.bump();
        }
        self.expect_punct(';', "to end the report statement");
        self.builder.finish_node();
    }

    /// `exit [loop-label] [when cond];` — `next` shares the exact shape.
    pub(super) fn parse_exit_next(&mut self) {
        self.start(SyntaxKind::ExitStmt);
        self.bump();
        // Optional loop label: `exit outer_loop when c;`
        if self.peek().kind == TokenKind::Ident && !self.at_kw("when") {
            self.bump();
        }
        if self.eat_kw("when") {
            self.parse_expression();
        }
        self.expect_punct(';', "to end the exit/next statement");
        self.builder.finish_node();
    }

    /// VHDL-2008: `force target <= expr;` / `release target;`
    pub(super) fn parse_force_release(&mut self) {
        self.start(SyntaxKind::SeqAssign);
        self.bump();
        self.parse_postfix_name();
        let op = self.peek_op2();
        if op.as_deref() == Some("<=") || op.as_deref() == Some(":=") {
            self.bump();
            self.bump();
            self.parse_expression();
        }
        self.expect_punct(';', "to end the force/release statement");
        self.builder.finish_node();
    }

    pub(super) fn parse_return_stmt(&mut self) {
        self.start(SyntaxKind::ReturnStmt);
        self.bump();
        if !self.at_punct(';') {
            self.parse_expression();
        }
        self.expect_punct(';', "to end the return statement");
        self.builder.finish_node();
    }

    /// VHDL-2008 parenthesized target list: `(a, b) := f(...);`
    pub(super) fn parse_seq_target_list(&mut self) {
        let mark = self.builder.mark();
        self.bump(); // (
        loop {
            self.parse_postfix_name();
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct(')', "to close the target list");
        if self.peek_op2().as_deref() == Some("<=") || self.peek_op2().as_deref() == Some(":=") {
            self.builder.start_node_at(mark, SyntaxKind::SeqAssign);
            self.bump();
            self.bump();
            self.parse_waveform_elements();
            self.expect_punct(';', "to end the assignment");
            self.builder.finish_node();
        } else {
            self.error_here("expected '<=' or ':=' after the target list");
            self.skip_to_semicolon();
        }
    }

    /// Assignment or procedure call: parse the target/name, then decide on
    /// the following operator (`<=` signal assignment, `:=` variable
    /// assignment with an optional conditional tail, else a call statement).
    pub(super) fn parse_seq_assign_or_call(&mut self) {
        let mark = self.builder.mark();
        self.parse_postfix_name();
        if self.peek_op2().as_deref() == Some("<=") {
            self.builder.start_node_at(mark, SyntaxKind::SeqAssign);
            self.bump();
            self.bump();
            self.parse_waveform_elements();
            self.expect_punct(';', "to end the signal assignment");
            self.builder.finish_node();
        } else if self.peek_op2().as_deref() == Some(":=") {
            self.builder.start_node_at(mark, SyntaxKind::VarAssign);
            self.bump();
            self.bump();
            self.parse_expression();
            // Conditional expression: `v := a when c else b;`
            loop {
                if self.at_kw("when") || self.at_kw("else") {
                    self.bump();
                    self.parse_expression();
                    continue;
                }
                break;
            }
            self.expect_punct(';', "to end the variable assignment");
            self.builder.finish_node();
        } else {
            self.builder.start_node_at(mark, SyntaxKind::CallStmt);
            self.expect_punct(';', "to end the statement");
            self.builder.finish_node();
        }
    }

    pub(super) fn parse_if(&mut self) {
        self.start(SyntaxKind::If);
        self.bump(); // if
        self.parse_expression();
        self.expect_kw("then", "after the if condition");
        self.parse_seq_block();
        while self.at_kw("elsif") {
            self.bump();
            self.parse_expression();
            self.expect_kw("then", "after the elsif condition");
            self.parse_seq_block();
        }
        if self.eat_kw("else") {
            self.parse_seq_block();
        }
        self.expect_kw("end", "to close the if");
        self.eat_kw("if");
        if self.peek().kind == TokenKind::Ident {
            self.bump(); // optional label
        }
        self.expect_punct(';', "to end the if statement");
        self.builder.finish_node();
    }

    /// Statements until a follow keyword (`elsif`/`else`/`end`).
    pub(super) fn parse_seq_block(&mut self) {
        while !self.at_kw("elsif")
            && !self.at_kw("else")
            && !self.at_kw("end")
            && !self.at_kw("when")
            && self.peek().kind != TokenKind::Eof
        {
            self.parse_seq_stmt();
        }
    }

    pub(super) fn parse_case(&mut self) {
        self.start(SyntaxKind::Case);
        self.bump(); // case
        if self.eat_punct('?') {
            // VHDL-2008 matching case: `case? expr is ...`
            self.start(SyntaxKind::Matching);
            self.builder.finish_node();
        }
        self.parse_expression();
        self.expect_kw("is", "after the case expression");
        while !self.at_kw("end") && self.peek().kind != TokenKind::Eof {
            self.parse_case_item();
        }
        self.expect_kw("end", "to close the case");
        self.eat_kw("case");
        self.eat_punct('?'); // `end case?;`
        if self.peek().kind == TokenKind::Ident {
            self.bump();
        }
        self.expect_punct(';', "to end the case statement");
        self.builder.finish_node();
    }

    /// One case alternative: `when [others | choices] => statements`, with
    /// range choices (`when 0 to 3 =>`) and `|`-separated choices.
    pub(super) fn parse_case_item(&mut self) {
        self.expect_kw("when", "before a case alternative");
        self.start(SyntaxKind::CaseItem);
        if self.eat_kw("others") {
            self.start(SyntaxKind::Others);
            self.builder.finish_node();
        } else {
            loop {
                self.parse_expression();
                if self.at_kw("to") || self.at_kw("downto") {
                    self.bump();
                    self.parse_expression(); // range choice: `when 0 to 3 =>`
                }
                if self.eat_punct('|') {
                    continue; // `when 0 | 1 =>`
                }
                if !self.eat_punct(',') {
                    break;
                }
            }
        }
        if self.peek_op2().as_deref() == Some("=>") {
            self.bump();
            self.bump();
        } else {
            self.error_here("expected '=>' in a case alternative");
        }
        while !self.at_kw("when") && !self.at_kw("end") && self.peek().kind != TokenKind::Eof {
            self.parse_seq_stmt();
        }
        self.builder.finish_node();
    }

    /// `for i in range loop ... end loop;`
    pub(super) fn parse_for_loop(&mut self) {
        self.start(SyntaxKind::For);
        self.bump(); // for
        self.expect_ident("for the loop variable");
        self.expect_kw("in", "after the loop variable");
        // Discrete range: `expr to/downto expr` (bounds may be attribute
        // expressions like `input'length-1`), `name'range`, or a subtype.
        self.start(SyntaxKind::Range);
        self.parse_range();
        self.builder.finish_node();
        self.expect_kw("loop", "after the discrete range");
        while !self.at_kw("end") && self.peek().kind != TokenKind::Eof {
            self.parse_seq_stmt();
        }
        self.expect_kw("end", "to close the loop");
        self.eat_kw("loop");
        if self.peek().kind == TokenKind::Ident {
            self.bump();
        }
        self.expect_punct(';', "to end the for loop");
        self.builder.finish_node();
    }

    pub(super) fn parse_loop(&mut self) {
        self.start(SyntaxKind::LoopStmt);
        if self.eat_kw("while") {
            self.parse_expression();
            self.expect_kw("loop", "after the while condition");
        } else {
            self.bump(); // loop
        }
        while !self.at_kw("end") && self.peek().kind != TokenKind::Eof {
            self.parse_seq_stmt();
        }
        self.expect_kw("end", "to close the loop");
        self.eat_kw("loop");
        if self.peek().kind == TokenKind::Ident {
            self.bump();
        }
        self.expect_punct(';', "to end the loop");
        self.builder.finish_node();
    }

    pub(super) fn parse_wait(&mut self) {
        self.start(SyntaxKind::Wait);
        self.bump(); // wait
                     // `wait [on sens] [until cond] [for time];` — the clauses may appear
                     // in any order (VHDL-2008 allows `wait on clk until rising_edge(clk)`).
        let mut seen_until = false;
        let mut seen_on = false;
        let mut seen_for = false;
        loop {
            if !seen_until && self.eat_kw("until") {
                seen_until = true;
                self.parse_expression();
            } else if !seen_on && self.eat_kw("on") {
                seen_on = true;
                self.start(SyntaxKind::SensList);
                loop {
                    self.parse_name();
                    if !self.eat_punct(',') {
                        break;
                    }
                }
                self.builder.finish_node();
            } else if !seen_for && self.eat_kw("for") {
                seen_for = true;
                self.parse_expression();
                // Physical literal unit: `10 ns` — the unit is a bare identifier.
                if self.peek().kind == TokenKind::Ident {
                    self.bump();
                }
            } else {
                break;
            }
        }
        self.expect_punct(';', "to end the wait statement");
        self.builder.finish_node();
    }
}
