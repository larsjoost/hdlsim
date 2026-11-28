//! Concurrent statements: the statement dispatch (labels, `postponed`),
//! signal assignment and concurrent procedure call (incl. aggregate
//! targets, conditional waveforms, delay mechanisms), selected signal
//! assignment, block statements with their VHDL-2002+ headers, process
//! statements, component/config instantiations, and the generate forms
//! (`for`, `if`, `case` incl. VHDL-2008 labeled bare-block alternatives).
//!
//! Child module of `vhdl::parser`, the same pattern as `decl.rs`. The label
//! of an instantiation is wrapped INTO the `Inst` node so the lowerer can
//! match configuration specifications by label; other statements keep it as
//! a sibling.

use crate::cst::Mark;
use crate::kinds::SyntaxKind;
use crate::token::TokenKind;

impl super::Parser<'_> {
    pub(super) fn parse_concurrent_stmt(&mut self) {
        // Optional label: `ident :`. For instantiations the label is wrapped
        // INTO the Inst node (so the lowerer can match configuration
        // specifications by label); other statements keep it as a sibling.
        let label_mark = if self.at_label() {
            let m = self.builder.mark();
            self.bump(); // label
            self.bump(); // ':'
            Some(m)
        } else {
            None
        };
        // VHDL-2008 `postponed process`
        if self.at_kw("postponed") {
            self.bump();
        }
        if self.at_kw("process") {
            self.parse_process();
        } else if self.at_kw("with") {
            self.parse_selected_assign();
        } else if self.at_kw("assert") {
            self.parse_assert(true);
        } else if self.at_kw("block") {
            self.parse_block();
        } else if self.at_kw("for") {
            self.parse_gen_for();
        } else if self.at_kw("if") {
            self.parse_gen_if();
        } else if self.at_kw("case") {
            self.parse_gen_case();
        } else if self.at_kw("begin") {
            self.parse_bare_block(label_mark);
        } else if self.at_kw("entity")
            || self.at_kw("component")
            || self.at_kw("configuration")
            || (self.peek().kind == TokenKind::Ident && self.peek2().kind == TokenKind::Ident)
        {
            self.parse_instantiation(label_mark);
        } else {
            // Concurrent signal assignment `target <= waveform ;` or a
            // concurrent procedure call `p(args);` — decided after the target
            // name, since `cmp_o(0) <= ...` starts like a call. The target may
            // also be an aggregate `(s1, s2, s3) <= v;`.
            self.parse_conc_assign_or_call();
        }
    }

    /// VHDL-2008 bare block as a generate-alternative body:
    /// `begin concurrent-stmts end [label] ;` (checked before the
    /// two-idents instantiation heuristic: `begin q <= ...`).
    pub(super) fn parse_bare_block(&mut self, label_mark: Option<Mark>) {
        match label_mark {
            Some(m) => self.builder.start_node_at(m, SyntaxKind::Block),
            None => self.start(SyntaxKind::Block),
        }
        self.bump(); // begin
        while !self.at_kw("end") && self.peek().kind != TokenKind::Eof {
            self.parse_concurrent_stmt();
        }
        self.expect_kw("end", "to close the block");
        if self.peek().kind == TokenKind::Ident {
            self.bump(); // optional label
        }
        self.expect_punct(';', "to end the block");
        self.builder.finish_node();
    }

    /// The dispatch tail: concurrent signal assignment or concurrent
    /// procedure call, decided only after the target name has been parsed
    /// (because `cmp_o(0) <= ...` starts like a call).
    pub(super) fn parse_conc_assign_or_call(&mut self) {
        let mark = self.builder.mark();
        self.parse_assign_target();
        // Concurrent configuration instantiation:
        // `lbl : configuration lib.cfg [generic map (...)] [port map (...)];`
        if self.at_kw("configuration") {
            self.bump();
            self.parse_name();
        }
        // Component instantiation with a selected name:
        // `lbl: lib.pkg.comp [generic map (...)] port map (...);`
        if self.at_kw("generic") || self.at_kw("port") {
            self.parse_selected_target_inst(mark);
            return;
        }
        if self.peek_op2().as_deref() != Some("<=") {
            self.builder.start_node_at(mark, SyntaxKind::CallStmt);
            self.expect_punct(';', "to end the concurrent procedure call");
            self.builder.finish_node();
            return;
        }
        self.builder.start_node_at(mark, SyntaxKind::ConcAssign);
        self.bump();
        self.bump(); // <=
                     // Waveform: `expr [after t] {, expr [after t]}`, and conditional
                     // form `expr when cond [else expr when cond]... [else expr]`.
        self.eat_delay_mechanism();
        self.parse_waveform_value();
        if self.at_kw("after") {
            self.bump();
            self.parse_waveform_value();
        }
        self.parse_conc_waveform_rest();
        self.expect_punct(';', "to end the concurrent assignment");
        self.builder.finish_node();
    }

    /// Component instantiation with a selected name:
    /// `lbl: lib.pkg.comp [generic map (...)] port map (...);`
    pub(super) fn parse_selected_target_inst(&mut self, mark: crate::cst::Mark) {
        self.builder.start_node_at(mark, SyntaxKind::Inst);
        if self.at_kw("generic") {
            self.bump();
            self.expect_kw("map", "after generic");
            self.parse_assoc_list();
        }
        if self.at_kw("port") {
            self.bump();
            self.expect_kw("map", "after port");
            self.parse_assoc_list();
        }
        self.expect_punct(';', "to end the instantiation");
        self.builder.finish_node();
    }

    /// Continuation of a concurrent waveform: conditional elements
    /// (`when cond else ...`) and further `,`-separated elements, each
    /// optionally delayed with `after`.
    pub(super) fn parse_conc_waveform_rest(&mut self) {
        loop {
            if self.at_kw("when") || self.at_kw("else") {
                self.bump();
                self.parse_waveform_value();
                if self.at_kw("after") {
                    self.bump();
                    self.parse_waveform_value();
                }
                continue;
            }
            if self.eat_punct(',') {
                self.eat_delay_mechanism();
                self.parse_waveform_value();
                if self.at_kw("after") {
                    self.bump();
                    self.parse_waveform_value();
                }
                continue;
            }
            break;
        }
    }

    /// Assignment target: a name or an aggregate `(s1, s2, ...)`.
    pub(super) fn parse_assign_target(&mut self) {
        if self.at_punct('(') {
            self.bump();
            loop {
                if self.peek().kind == TokenKind::Ident {
                    self.parse_name();
                }
                if !self.eat_punct(',') {
                    break;
                }
            }
            self.expect_punct(')', "to close the aggregate target");
        } else {
            self.parse_name();
        }
    }

    /// `with sel select target <= w1 when c1, w2 when c2, ...;`
    pub(super) fn parse_selected_assign(&mut self) {
        self.start(SyntaxKind::SelectedAssign);
        self.bump(); // with
        self.parse_expression(); // selector
        self.expect_kw("select", "in the selected signal assignment");
        // Target may be an aggregate `(s1, s2, ...)`.
        self.parse_assign_target();
        if self.peek_op2().as_deref() == Some("<=") {
            self.bump();
            self.bump();
        } else {
            self.error_here("expected '<=' in the selected signal assignment");
        }
        loop {
            self.parse_waveform_value(); // waveform value
            if self.at_kw("after") {
                self.bump();
                self.parse_waveform_value();
            }
            self.expect_kw("when", "before the selection choices");
            if self.at_kw("others") {
                self.start(SyntaxKind::Others);
                self.bump();
                self.builder.finish_node();
            } else {
                loop {
                    self.parse_waveform_value(); // choice
                    if !self.eat_punct('|') {
                        break; // a following `,` separates waveforms, not choices
                    }
                }
            }
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct(';', "to end the selected signal assignment");
        self.builder.finish_node();
    }

    /// `[label :] case expr is when choices => concurrent-stmts ... end generate;`
    pub(super) fn parse_gen_case(&mut self) {
        self.start(SyntaxKind::GenCase);
        self.bump(); // case
        self.parse_expression();
        self.eat_kw("is"); // tolerated though not standard
        self.expect_kw("generate", "after the case-generate expression");
        while !self.at_kw("end") && self.peek().kind != TokenKind::Eof {
            self.parse_gen_case_item();
        }
        self.expect_kw("end", "to close the case-generate");
        self.eat_kw("case"); // `end case generate;` / `end generate;`
        self.eat_kw("generate");
        if self.peek().kind == TokenKind::Ident {
            self.bump(); // optional label
        }
        self.expect_punct(';', "to end the case-generate");
        self.builder.finish_node();
    }

    /// One case-generate alternative: `when [lbl :] choices => body`. The
    /// body is a bare block (`begin ... end lbl;`) for labeled alternatives,
    /// otherwise declarations plus concurrent statements.
    pub(super) fn parse_gen_case_item(&mut self) {
        self.expect_kw("when", "before a case-generate alternative");
        self.start(SyntaxKind::CaseItem);
        let labeled = self.parse_gen_case_choices();
        if self.peek_op2().as_deref() == Some("=>") {
            self.bump();
            self.bump();
        } else {
            self.error_here("expected '=>' in a case-generate alternative");
        }
        if labeled && self.at_kw("begin") {
            self.parse_bare_block(None);
        } else {
            self.parse_gen_decl_part();
            while !self.at_kw("when") && !self.at_kw("end") && self.peek().kind != TokenKind::Eof {
                self.parse_concurrent_stmt();
            }
        }
        self.builder.finish_node();
    }

    /// The choices of a case-generate alternative; returns whether the
    /// alternative carried a VHDL-2008 label (`when lbl : choice =>`).
    pub(super) fn parse_gen_case_choices(&mut self) -> bool {
        if self.at_kw("others") {
            self.start(SyntaxKind::Others);
            self.bump();
            self.builder.finish_node();
            return false;
        }
        // VHDL-2008 alternative label: `when lbl : choice =>`.
        let mut labeled = false;
        if self.peek().kind == TokenKind::Ident
            && self.peek2_text() == ":"
            && self.peek3_text() != "="
        {
            self.bump();
            self.bump();
            labeled = true;
        }
        loop {
            self.parse_expression();
            if self.eat_punct('|') {
                continue;
            }
            if !self.eat_punct(',') {
                break;
            }
        }
        labeled
    }

    /// `block [ (guard) ] [guarded] [is] decls begin concurrent-stmts end block [label] ;`
    pub(super) fn parse_block(&mut self) {
        self.start(SyntaxKind::Block);
        self.bump(); // block
        if self.at_punct('(') {
            self.bump();
            self.parse_expression();
            self.expect_punct(')', "to close the block guard");
        }
        if self.eat_kw("guarded") {
            // VHDL-2008 guarded block — keyword kept in the tree
        }
        self.eat_kw("is");
        self.parse_block_header_decls();
        self.expect_kw("begin", "in block");
        while !self.at_kw("end") && self.peek().kind != TokenKind::Eof {
            self.parse_concurrent_stmt();
        }
        self.expect_kw("end", "to close the block");
        self.eat_kw("block");
        if self.peek().kind == TokenKind::Ident {
            self.bump(); // optional label
        }
        self.expect_punct(';', "to end the block");
        self.builder.finish_node();
    }

    /// Sequential block statement:
    /// `[label:] block [(guard)] [is] <decls> begin <seq stmts> end block [label];`
    pub(super) fn parse_block_stmt(&mut self) {
        self.start(SyntaxKind::Block);
        self.bump(); // block
        if self.at_punct('(') {
            self.bump();
            self.parse_expression();
            self.expect_punct(')', "to close the block guard");
        }
        if self.eat_kw("guarded") {
            // VHDL-2008 guarded block — keyword kept in the tree
        }
        self.eat_kw("is");
        self.parse_block_header_decls();
        self.expect_kw("begin", "in block");
        while !self.at_kw("end") && self.peek().kind != TokenKind::Eof {
            self.parse_seq_stmt();
        }
        self.expect_kw("end", "to close the block");
        self.eat_kw("block");
        if self.peek().kind == TokenKind::Ident {
            self.bump(); // optional label
        }
        self.expect_punct(';', "to end the block");
        self.builder.finish_node();
    }

    /// Block header items up to `begin`: the VHDL-2002+ `generic (...);` /
    /// `port (...);` clauses, their `generic map` / `port map` header
    /// counterparts, and plain declarations.
    pub(super) fn parse_block_header_decls(&mut self) {
        while !self.at_kw("begin") && !self.at_kw("end") && self.peek().kind != TokenKind::Eof {
            if self.at_kw("generic") && self.peek2_text() == "(" {
                self.parse_generic_clause();
            } else if self.at_kw("port") && self.peek2_text() == "(" {
                self.parse_port_clause();
            } else if (self.at_kw("generic") || self.at_kw("port"))
                && self.peek2_text().eq_ignore_ascii_case("map")
            {
                self.bump(); // generic | port
                self.bump(); // map
                self.parse_assoc_list();
                self.eat_punct(';');
            } else {
                self.parse_decl();
            }
        }
    }

    /// True when the next token starts a declarative item (as opposed to a
    /// concurrent statement) inside a generate body. `for` is excluded: it
    /// would be ambiguous with a nested for-generate.
    pub(super) fn at_decl_start(&self) -> bool {
        if self.peek().kind != TokenKind::Ident {
            return false;
        }
        matches!(
            self.peek_text().to_ascii_lowercase().as_str(),
            "signal"
                | "constant"
                | "variable"
                | "type"
                | "subtype"
                | "component"
                | "function"
                | "procedure"
                | "shared"
                | "alias"
                | "attribute"
                | "file"
                | "use"
                | "package"
        )
    }

    /// Declarative part of a generate body: declarations up to an optional
    /// `begin` keyword (required by VHDL only when declarations are present).
    pub(super) fn parse_gen_decl_part(&mut self) {
        while self.at_decl_start() && self.peek().kind != TokenKind::Eof {
            self.parse_decl();
        }
        self.eat_kw("begin");
    }

    /// `[label :] for var in range generate [decls begin] concurrent-stmts end generate [label] ;`
    pub(super) fn parse_gen_for(&mut self) {
        self.start(SyntaxKind::GenFor);
        self.bump(); // for
        if self.peek().kind == TokenKind::Ident {
            self.parse_name();
        }
        self.expect_kw("in", "in for-generate");
        if self.at_kw("range") {
            self.bump();
            if self.at_punct('<') {
                self.bump();
                self.bump(); // range <>
            } else {
                self.parse_expression();
                if self.at_kw("to") || self.at_kw("downto") {
                    self.bump();
                    self.parse_expression();
                }
            }
        } else {
            self.parse_expression();
            if self.at_kw("to") || self.at_kw("downto") {
                self.bump();
                self.parse_expression();
            }
        }
        self.expect_kw("generate", "in for-generate");
        self.parse_gen_decl_part();
        while !self.at_kw("end") && self.peek().kind != TokenKind::Eof {
            self.parse_concurrent_stmt();
        }
        self.expect_kw("end", "to close for-generate");
        self.eat_kw("for"); // `end for generate;` / `end generate;`
        self.eat_kw("generate");
        if self.peek().kind == TokenKind::Ident {
            self.bump(); // optional label
        }
        self.expect_punct(';', "to end for-generate");
        self.builder.finish_node();
    }

    /// `[label :] if cond generate stmts [elsif cond generate stmts]* [else generate stmts] end generate [label] ;`
    pub(super) fn parse_gen_if(&mut self) {
        self.start(SyntaxKind::GenIf);
        self.bump(); // if
        self.parse_expression();
        self.expect_kw("generate", "in if-generate");
        loop {
            self.parse_gen_decl_part();
            while !self.at_kw("end")
                && !self.at_kw("elsif")
                && !self.at_kw("else")
                && self.peek().kind != TokenKind::Eof
            {
                self.parse_concurrent_stmt();
            }
            if self.at_kw("elsif") {
                self.bump();
                self.parse_expression();
                self.expect_kw("generate", "in elsif-generate");
                continue;
            }
            if self.at_kw("else") {
                self.bump();
                self.expect_kw("generate", "in else-generate");
                self.parse_gen_decl_part();
                while !self.at_kw("end") && self.peek().kind != TokenKind::Eof {
                    self.parse_concurrent_stmt();
                }
            }
            break;
        }
        self.expect_kw("end", "to close if-generate");
        self.eat_kw("if"); // `end if generate;` / `end generate;`
        self.eat_kw("generate");
        if self.peek().kind == TokenKind::Ident {
            self.bump(); // optional label
        }
        self.expect_punct(';', "to end if-generate");
        self.builder.finish_node();
    }

    pub(super) fn parse_process(&mut self) {
        self.start(SyntaxKind::Process);
        self.bump(); // process
        if self.eat_punct('(') {
            self.start(SyntaxKind::SensList);
            while !self.at_punct(')') && self.peek().kind != TokenKind::Eof {
                self.parse_name();
                if !self.eat_punct(',') {
                    break;
                }
            }
            self.expect_punct(')', "to close the sensitivity list");
            self.builder.finish_node();
        }
        self.eat_kw("is");
        while !self.at_kw("begin") && self.peek().kind != TokenKind::Eof {
            self.parse_decl();
        }
        self.expect_kw("begin", "before the process statements");
        while !self.at_kw("end") && self.peek().kind != TokenKind::Eof {
            self.parse_seq_stmt();
        }
        self.expect_kw("end", "to close the process");
        self.eat_kw("postponed"); // `end postponed process;`
        self.eat_kw("process");
        if self.peek().kind == TokenKind::Ident {
            self.bump(); // optional label
        }
        self.expect_punct(';', "to end the process");
        self.builder.finish_node();
    }

    /// `label : comp [generic map (...)] port map (...);` or `entity lib.name(arch) ...`.
    pub(super) fn parse_instantiation(&mut self, label: Option<Mark>) {
        match label {
            // Wrap the already-bumped `label :` into the Inst node.
            Some(m) => self.builder.start_node_at(m, SyntaxKind::Inst),
            None => self.start(SyntaxKind::Inst),
        }
        if self.eat_kw("entity") {
            self.parse_name(); // lib.entity
            if self.eat_punct('(') {
                if self.peek().kind == TokenKind::Ident {
                    self.bump(); // architecture name
                }
                self.expect_punct(')', "to close the architecture selection");
            }
        } else if self.eat_kw("component") {
            self.parse_name();
        } else if self.eat_kw("configuration") {
            self.parse_name(); // configuration aspect
        } else {
            self.parse_name(); // component name (the label was consumed by the caller)
        }
        if self.at_kw("generic") {
            self.bump();
            self.expect_kw("map", "after generic");
            self.parse_assoc_list();
        }
        if self.at_kw("port") {
            self.bump();
            self.expect_kw("map", "after port");
            self.parse_assoc_list();
        }
        self.expect_punct(';', "to end the instantiation");
        self.builder.finish_node();
    }
}
