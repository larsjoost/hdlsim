//! Verilog procedural statements: block/if/case (with `unique`/`priority`/
//! `inside`), loops, `fork`/`join`, `disable`, event controls, `#delay`
//! prefixes, procedural assignments with intra-assignment delays, inc/dec,
//! `force`/`deassign`/`release`, immediate assertions, event triggers, and
//! system task calls. Child module of `verilog::parser` (house pattern:
//! cohesive grammar slices live in child modules to keep the parser file at
//! its standards baseline).

use super::DECL_KEYWORDS;
use crate::kinds::SyntaxKind;
use crate::token::TokenKind;

impl super::Parser<'_> {
    pub(super) fn parse_statement(&mut self) {
        self.eat_attributes();
        let text = self.peek_text().to_string();
        match self.peek().kind {
            TokenKind::Ident => self.parse_stmt_ident(&text),
            TokenKind::Punct => self.parse_stmt_punct(&text),
            TokenKind::SysTask => self.parse_syscall_stmt(),
            _ => self.parse_stmt_expr(),
        }
    }

    /// Keyword-led statements. Arm order mirrors the grammar dispatch (the
    /// inc/dec shape sits between the keyword arms exactly as before, so
    /// recovery on malformed input builds the same tree).
    fn parse_stmt_ident(&mut self, text: &str) {
        match text {
            // `assign` is legal inside generate blocks (module item as statement).
            "assign" => self.parse_assign(),
            "foreach" => self.parse_foreach(),
            "import" => self.parse_import(false),
            "export" => self.parse_import(true),
            // `deassign lhs;` — procedural deassignment (LRM 9.2.2). `release`
            // parses and drops (force/release are not simulated).
            "deassign" => self.parse_deassign(),
            "release" => self.parse_release(),
            // `x++;` / `x--;` (an identifier followed by ++/--).
            _ if self.peek2_text() == "+" && self.peek3_text() == "+"
                || self.peek2_text() == "-" && self.peek3_text() == "-" =>
            {
                self.parse_incdec_stmt(false);
            }
            "force" => self.parse_force(),
            "begin" => self.parse_block(),
            "if" => self.parse_if_stmt(),
            "case" | "casez" | "casex" => self.parse_case(),
            // Case qualifiers bind to the following case statement.
            "unique" | "unique0" | "priority" => self.parse_case_qualifier(),
            "while" => self.parse_while(),
            "break" => self.parse_break(),
            "continue" => self.parse_continue(),
            "repeat" => self.parse_repeat(),
            "forever" => self.parse_forever(),
            "do" => self.parse_do_while(),
            // `wait (expr) stmt` — procedural wait on a condition.
            "wait" => self.parse_wait_stmt(),
            "disable" => self.parse_disable(),
            "fork" => self.parse_fork_join(),
            // SV immediate assertions: `assert (e) [else stmt];` etc.
            "assert" | "assume" | "cover" | "expect" => self.parse_immediate_assert(),
            // Procedural for: `for (init; cond; step) stmt`. Init and step
            // are assignments without their own trailing `;`.
            "for" => self.parse_for(),
            // `return [expr];` in functions.
            "return" => self.parse_return(),
            // Module-level constructs are also legal inside generate blocks.
            "always" | "always_comb" | "always_ff" | "always_latch" => self.parse_always(),
            "initial" => self.parse_initial(),
            _ => self.parse_stmt_ident_other(text),
        }
    }

    /// Remaining identifier-led statements: a block-level declaration
    /// (`int x = 3;` inside begin/end) or an assignment / expression
    /// statement.
    fn parse_stmt_ident_other(&mut self, text: &str) {
        if DECL_KEYWORDS.contains(&text)
            || (self.peek2().kind == TokenKind::Ident && !self.instantiation_ahead())
        {
            self.parse_decl();
            return;
        }
        self.parse_stmt_expr();
    }

    /// Punctuation-led statements.
    fn parse_stmt_punct(&mut self, text: &str) {
        // `++x;` / `--x;`.
        if text == "+" && self.peek2_text() == "+" || text == "-" && self.peek2_text() == "-" {
            self.parse_incdec_stmt(true);
            return;
        }
        // Event trigger: `-> name;` / `->> name;`.
        if text == "-" && self.peek2_text() == ">" {
            self.parse_trigger();
            return;
        }
        match text {
            ";" => {
                self.bump(); // empty statement
            }
            "#" => {
                // `#delay stmt` — the delayed statement is a child of Delay.
                self.builder.start_node(SyntaxKind::Delay);
                self.bump();
                self.parse_primary(); // delay value
                self.parse_statement(); // delayed statement
                self.builder.finish_node();
            }
            "@" => {
                // Event wait: `@(posedge clk) stmt`.
                self.builder.start_node(SyntaxKind::Wait);
                self.bump();
                self.parse_sens_list();
                self.parse_statement();
                self.builder.finish_node();
            }
            _ => self.parse_stmt_expr(),
        }
    }

    /// `x++;` / `x--;` as a whole statement (`prefix` for `++x;` / `--x;`).
    fn parse_incdec_stmt(&mut self, prefix: bool) {
        self.builder.start_node(SyntaxKind::StmtAssign);
        if prefix {
            self.bump();
            self.bump();
        }
        self.parse_postfix(); // consumes the postfix `++`/`--` too
        self.builder.finish_node();
        self.expect_punct(';', "to end the increment/decrement");
    }

    /// `deassign lhs;` (LRM 9.2.2).
    fn parse_deassign(&mut self) {
        self.builder.start_node(SyntaxKind::DeassignStmt);
        self.bump();
        self.parse_expression();
        self.expect_punct(';', "to end the statement");
        self.builder.finish_node();
    }

    /// `release lhs;` — parsed and dropped (not simulated).
    fn parse_release(&mut self) {
        self.bump();
        self.parse_expression();
        self.expect_punct(';', "to end the statement");
    }

    /// `force lhs = rhs;` — parsed and dropped (not simulated).
    fn parse_force(&mut self) {
        self.bump();
        self.parse_expression();
        if self.eat_punct('=') {
            self.parse_expression();
        }
        self.expect_punct(';', "to end the force statement");
    }

    /// `begin [: name] stmts... end`.
    fn parse_block(&mut self) {
        self.builder.start_node(SyntaxKind::Block);
        self.bump();
        // Optional `: name`.
        if self.eat_punct(':') && self.peek().kind == TokenKind::Ident {
            self.builder.start_node(SyntaxKind::Name);
            self.bump();
            self.builder.finish_node();
        }
        while !self.at_kw("end") && self.peek().kind != TokenKind::Eof {
            self.parse_statement();
        }
        if !self.eat_kw("end") {
            self.error_here("expected `end`");
        }
        self.builder.finish_node();
    }

    /// `if (cond) stmt [else stmt]`.
    fn parse_if_stmt(&mut self) {
        self.builder.start_node(SyntaxKind::If);
        self.bump();
        self.expect_punct('(', "after `if`");
        self.parse_expression();
        self.expect_punct(')', "to close the if condition");
        self.parse_statement();
        if self.eat_kw("else") {
            self.parse_statement();
        }
        self.builder.finish_node();
    }

    /// `unique[/0]|priority case...endcase` with the qualifiers in the node.
    fn parse_case_qualifier(&mut self) {
        self.builder.start_node(SyntaxKind::Case);
        while self.at_kw("unique") || self.at_kw("unique0") || self.at_kw("priority") {
            self.bump();
        }
        self.parse_case_tail();
        self.builder.finish_node();
    }

    /// `while (cond) stmt`.
    fn parse_while(&mut self) {
        self.builder.start_node(SyntaxKind::While);
        self.bump();
        self.expect_punct('(', "after `while`");
        self.parse_expression();
        self.expect_punct(')', "to close the while condition");
        self.parse_statement();
        self.builder.finish_node();
    }

    /// `break;` (SV).
    fn parse_break(&mut self) {
        self.builder.start_node(SyntaxKind::BreakStmt);
        self.bump();
        self.eat_punct(';');
        self.builder.finish_node();
    }

    /// `continue;` (SV).
    fn parse_continue(&mut self) {
        self.builder.start_node(SyntaxKind::ContinueStmt);
        self.bump();
        self.eat_punct(';');
        self.builder.finish_node();
    }

    /// `repeat (count) stmt`.
    fn parse_repeat(&mut self) {
        self.builder.start_node(SyntaxKind::Repeat);
        self.bump();
        self.expect_punct('(', "after `repeat`");
        self.parse_expression();
        self.expect_punct(')', "to close the repeat count");
        self.parse_statement();
        self.builder.finish_node();
    }

    /// `forever stmt`.
    fn parse_forever(&mut self) {
        self.builder.start_node(SyntaxKind::Forever);
        self.bump();
        self.parse_statement();
        self.builder.finish_node();
    }

    /// `do stmt while (cond);`.
    fn parse_do_while(&mut self) {
        self.builder.start_node(SyntaxKind::DoWhile);
        self.bump(); // do
        self.parse_statement();
        if !self.eat_kw("while") {
            self.error_here("expected `while` after a `do` body");
        }
        self.expect_punct('(', "after `while`");
        self.parse_expression();
        self.expect_punct(')', "to close the do-while condition");
        self.expect_punct(';', "to end the do-while statement");
        self.builder.finish_node();
    }

    /// `wait (cond) stmt`.
    fn parse_wait_stmt(&mut self) {
        self.builder.start_node(SyntaxKind::Wait);
        self.bump();
        self.expect_punct('(', "after `wait`");
        self.parse_expression();
        self.expect_punct(')', "to close the wait condition");
        self.parse_statement();
        self.builder.finish_node();
    }

    /// `disable [hierarchical.]name;`.
    fn parse_disable(&mut self) {
        self.builder.start_node(SyntaxKind::Disable);
        self.bump();
        if self.peek().kind == TokenKind::Ident {
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
        }
        self.expect_punct(';', "to end the disable statement");
        self.builder.finish_node();
    }

    /// `fork [: name] stmts... join[_any|_none]`.
    fn parse_fork_join(&mut self) {
        self.builder.start_node(SyntaxKind::ForkJoin);
        self.bump(); // fork
        if self.eat_punct(':') && self.peek().kind == TokenKind::Ident {
            self.builder.start_node(SyntaxKind::Name);
            self.bump();
            self.builder.finish_node();
        }
        while !self.at_kw("join")
            && !self.at_kw("join_any")
            && !self.at_kw("join_none")
            && self.peek().kind != TokenKind::Eof
        {
            self.parse_statement();
        }
        if !(self.eat_kw("join") || self.eat_kw("join_any") || self.eat_kw("join_none")) {
            self.error_here("expected `join`, `join_any`, or `join_none`");
        }
        self.builder.finish_node();
    }

    /// Immediate assertion: `assert|assume|cover|expect (e) [else stmt];`.
    /// A deferred `assert property (...)` takes the parenthesized property
    /// expression without an `else`.
    fn parse_immediate_assert(&mut self) {
        self.builder.start_node(SyntaxKind::SvaStmt);
        self.bump();
        if self.eat_kw("property") {
            // Deferred assertion: `assert property (@(posedge clk) e);`
            if self.eat_punct('(') {
                if !self.at_punct(')') {
                    self.parse_expression();
                }
                self.expect_punct(')', "to close the property expression");
            }
        } else if self.eat_punct('(') {
            if !self.at_punct(')') {
                self.parse_expression();
            }
            self.expect_punct(')', "to close the assertion condition");
        }
        if self.eat_kw("else") {
            // The else branch is a full statement and consumes its own `;`.
            self.parse_statement();
        } else {
            self.expect_punct(';', "to end the assertion statement");
        }
        self.builder.finish_node();
    }

    /// Event trigger: `-> name;` / `->> name;`.
    fn parse_trigger(&mut self) {
        self.builder.start_node(SyntaxKind::Trigger);
        self.bump(); // -
        self.bump(); // >
        if self.at_punct('>') {
            self.bump(); // ->>
        }
        if self.peek().kind == TokenKind::Ident {
            self.builder.start_node(SyntaxKind::Name);
            self.bump();
            self.builder.finish_node();
        } else {
            self.error_here("expected an event name after `->`");
        }
        self.expect_punct(';', "to end the event trigger");
        self.builder.finish_node();
    }

    /// `for (init; cond; step) stmt` — init/step are assignments without
    /// their own trailing `;`.
    fn parse_for(&mut self) {
        self.builder.start_node(SyntaxKind::For);
        self.bump();
        self.expect_punct('(', "after `for`");
        if !self.at_punct(';') {
            self.parse_assign_no_semi();
        }
        self.expect_punct(';', "in for header");
        self.parse_expression(); // condition
        self.expect_punct(';', "in for header");
        if !self.at_punct(')') {
            self.parse_assign_no_semi();
        }
        self.expect_punct(')', "to close the for header");
        self.parse_statement(); // body
        self.builder.finish_node();
    }

    /// `return [expr];`.
    fn parse_return(&mut self) {
        self.builder.start_node(SyntaxKind::ReturnStmt);
        self.bump();
        if !self.at_punct(';') {
            self.parse_expression();
        }
        self.expect_punct(';', "to end the return statement");
        self.builder.finish_node();
    }

    /// System task call statement: `$display(...);`.
    fn parse_syscall_stmt(&mut self) {
        self.builder.start_node(SyntaxKind::SysCall);
        self.bump();
        if self.eat_punct('(') {
            if !self.at_punct(')') {
                loop {
                    self.parse_call_arg();
                    if !self.eat_punct(',') {
                        break;
                    }
                }
            }
            self.expect_punct(')', "to close the system task call");
        }
        self.expect_punct(';', "to end the system task call");
        self.builder.finish_node();
    }

    /// Procedural assignment (`lhs = expr;` / `lhs <= expr;`) or an
    /// expression statement (`f(...);`). The lhs is parsed as a
    /// postfix expression only, so `<=` is read as non-blocking
    /// assignment, not as less-than-or-equal.
    fn parse_stmt_expr(&mut self) {
        self.builder.start_node(SyntaxKind::StmtAssign);
        let mark = self.builder.mark();
        self.parse_postfix();
        if self.parse_assign_op() {
            // Intra-assignment delay: `x <= #100 y;` / `x = #1 y;`.
            self.parse_intra_assign_delay();
            self.parse_expression();
        } else {
            self.parse_binops(mark, 1);
        }
        self.expect_punct(';', "to end the statement");
        self.builder.finish_node();
    }

    /// The `#delay` between the assignment operator and the rhs, inside the
    /// open StmtAssign: `#(...)` comma list or a single primary.
    fn parse_intra_assign_delay(&mut self) {
        if !self.at_punct('#') {
            return;
        }
        self.builder.start_node(SyntaxKind::Delay);
        self.bump();
        self.parse_delay_value("to close the assignment delay");
        self.builder.finish_node();
    }
    fn parse_case(&mut self) {
        self.builder.start_node(SyntaxKind::Case);
        self.parse_case_tail();
        self.builder.finish_node();
    }

    /// `case...endcase` with the Case node already open (qualifiers included).
    fn parse_case_tail(&mut self) {
        self.bump(); // case/casez/casex
        self.expect_punct('(', "after `case`");
        self.parse_expression();
        self.expect_punct(')', "to close the case expression");
        if self.eat_kw("inside") {
            // SystemVerilog `case (expr) inside [...]`
        }
        loop {
            if self.at_kw("endcase") || self.peek().kind == TokenKind::Eof {
                break;
            }
            self.builder.start_node(SyntaxKind::CaseItem);
            if self.at_kw("default") {
                self.bump();
            } else {
                self.parse_case_labels();
            }
            self.expect_punct(':', "before the case item body");
            self.parse_statement();
            self.builder.finish_node();
        }
        if !self.eat_kw("endcase") {
            self.error_here("expected `endcase`");
        }
        // No finish_node here: the caller (parse_case / qualifier arm) owns the
        // Case node and closes it.
    }

    /// The labels before a case item's `:` — plain expressions, or `inside`
    /// patterns `[lo:hi]` / `[a, b]`.
    fn parse_case_labels(&mut self) {
        loop {
            if self.at_punct('[') {
                // `case inside` pattern: `[lo:hi]` or `[a, b]`.
                self.bump();
                loop {
                    self.parse_expression();
                    if self.eat_punct(':') {
                        self.parse_expression(); // range inside the pattern
                    }
                    if !self.eat_punct(',') {
                        break;
                    }
                }
                self.expect_punct(']', "to close the inside pattern");
            } else {
                self.parse_expression();
            }
            if !self.eat_punct(',') {
                break;
            }
        }
    }

    /// `foreach ( arr[i] ) stmt` / `foreach ( arr[i, j] ) stmt`.
    fn parse_foreach(&mut self) {
        self.builder.start_node(SyntaxKind::Foreach);
        self.bump(); // foreach
        self.expect_punct('(', "after foreach");
        self.parse_expression(); // array (possibly with [idx] selects)
        self.expect_punct(')', "to close the foreach header");
        self.parse_statement();
        self.builder.finish_node();
    }
}
