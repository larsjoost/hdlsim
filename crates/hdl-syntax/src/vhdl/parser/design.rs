//! Design units: the design-file loop (library/use clauses, entities,
//! architectures, packages incl. VHDL-2008 instantiations, configurations,
//! context declarations), the generic/port clause headers, and component
//! configuration specifications ([`Parser::parse_for_spec`]).
//!
//! Child module of `vhdl::parser`, the same pattern as `lex.rs`. Unknown
//! constructs produce a diagnostic and skip to the next `;` so one bad unit
//! does not kill the rest of the file.

use crate::kinds::SyntaxKind;
use crate::token::TokenKind;

impl super::Parser<'_> {
    pub(super) fn parse_design_file(&mut self) {
        self.builder.start_node(SyntaxKind::Root);
        while self.peek().kind != TokenKind::Eof {
            if self.at_kw("library") {
                self.parse_library_clause();
            } else if self.at_kw("use") {
                self.parse_use_clause();
            } else if self.at_kw("entity") {
                self.parse_entity();
            } else if self.at_kw("architecture") {
                self.parse_architecture();
            } else if self.at_kw("package") {
                // VHDL-2008 package instantiation is also a design unit:
                // `package p is new g ...;`
                if self.peek3_text().eq_ignore_ascii_case("is")
                    && self.peek4_text().eq_ignore_ascii_case("new")
                {
                    self.parse_package_inst();
                } else {
                    self.parse_package();
                }
            } else if self.at_kw("configuration") {
                self.parse_configuration();
            } else if self.at_kw("context") {
                // `context name;` is a context reference clause;
                // `context name is ... end context;` is a context declaration.
                if self.peek3_text().eq_ignore_ascii_case("is") {
                    self.parse_context_decl();
                } else {
                    self.parse_context_clause();
                }
            } else {
                self.error_here(
                    "expected a design unit (entity/architecture/package) or context clause",
                );
                self.bump();
            }
        }
        while self.pos < self.tokens.len() {
            self.bump();
        }
        self.builder.finish_node();
    }

    /// VHDL-2008 context clause: `context lib.name[, ...];`
    pub(super) fn parse_context_clause(&mut self) {
        self.start(SyntaxKind::ContextClause);
        self.bump(); // context
        loop {
            self.parse_name();
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct(';', "to end the context clause");
        self.builder.finish_node();
    }

    /// Context declaration: `context name is <context clause items> end [context] [name];`
    pub(super) fn parse_context_decl(&mut self) {
        self.start(SyntaxKind::ContextDecl);
        self.bump(); // context
        self.expect_ident("for the context name");
        self.expect_kw("is", "in the context declaration");
        while !self.at_kw("end") && self.peek().kind != TokenKind::Eof {
            if self.at_kw("library") {
                self.parse_library_clause();
            } else if self.at_kw("use") {
                self.parse_use_clause();
            } else if self.at_kw("context") {
                self.parse_context_clause();
            } else {
                self.error_here("expected a context clause item in the context declaration");
                self.skip_to_semicolon();
            }
        }
        self.expect_kw("end", "to close the context declaration");
        self.eat_kw("context");
        if self.peek().kind == TokenKind::Ident {
            self.bump(); // optional context name
        }
        self.expect_punct(';', "to end the context declaration");
        self.builder.finish_node();
    }

    /// `configuration name of entity is { block_config } end [configuration] [name];`
    pub(super) fn parse_configuration(&mut self) {
        self.start(SyntaxKind::ConfigDecl);
        self.bump(); // configuration
        self.expect_ident("for the configuration name");
        self.expect_kw("of", "in the configuration declaration");
        self.parse_name(); // entity (possibly selected)
        self.expect_kw("is", "in the configuration declaration");
        while !self.at_kw("end") && self.peek().kind != TokenKind::Eof {
            if self.at_kw("for") {
                self.parse_config_for();
            } else {
                self.error_here("expected a `for` block configuration");
                self.skip_to_semicolon();
            }
        }
        self.expect_kw("end", "to close the configuration declaration");
        self.eat_kw("configuration");
        if self.peek().kind == TokenKind::Ident {
            self.bump(); // optional configuration name
        }
        self.expect_punct(';', "to end the configuration declaration");
        self.builder.finish_node();
    }

    /// One `for <block spec> ... end for;` inside a configuration declaration.
    /// Block spec: `arch_name`, `label : comp`, `all : comp`, `others : comp`.
    pub(super) fn parse_config_for(&mut self) {
        self.start(SyntaxKind::ForSpec);
        self.bump(); // for
        if self.at_kw("all") || self.at_kw("others") {
            self.bump();
        } else if self.peek().kind == TokenKind::Ident {
            self.parse_name();
        } else {
            self.error_here("expected a block specification in the block configuration");
        }
        if self.eat_punct(':') {
            self.parse_name(); // component name
        }
        while !self.at_kw("end") && self.peek().kind != TokenKind::Eof {
            if self.at_kw("for") {
                self.parse_config_for(); // nested block configuration
            } else if self.at_kw("use") {
                self.parse_config_binding();
            } else {
                self.bump();
            }
        }
        self.expect_kw("end", "to close the configuration for-block");
        self.eat_kw("for");
        self.expect_punct(';', "to end the configuration for-block");
        self.builder.finish_node();
    }

    /// Binding indication inside a configuration declaration:
    /// `use [entity|configuration] name [(arch)] [generic map (...)] [port map (...)];`
    pub(super) fn parse_config_binding(&mut self) {
        self.start(SyntaxKind::UseClause);
        self.bump(); // use
        if self.at_kw("entity") || self.at_kw("configuration") {
            self.bump();
        }
        self.parse_name(); // lib.unit / configuration name
        if self.eat_punct('(') {
            if self.peek().kind == TokenKind::Ident {
                self.bump(); // architecture name
            }
            self.expect_punct(')', "to close the architecture selection");
        }
        if self.at_kw("generic") {
            self.bump();
            self.expect_kw("map", "after generic in a binding indication");
            self.parse_assoc_list();
        }
        if self.at_kw("port") {
            self.bump();
            self.expect_kw("map", "after port in a binding indication");
            self.parse_assoc_list();
        }
        self.expect_punct(';', "to end the binding indication");
        self.builder.finish_node();
    }

    pub(super) fn parse_library_clause(&mut self) {
        self.start(SyntaxKind::LibraryClause);
        self.bump(); // library
        while self.peek().kind == TokenKind::Ident {
            self.parse_name();
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct(';', "to end the library clause");
        self.builder.finish_node();
    }

    pub(super) fn parse_use_clause(&mut self) {
        self.start(SyntaxKind::UseClause);
        self.bump(); // use
        loop {
            self.parse_name();
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct(';', "to end the use clause");
        self.builder.finish_node();
    }

    pub(super) fn parse_entity(&mut self) {
        self.start(SyntaxKind::Entity);
        self.bump(); // entity
        self.expect_ident("for the entity name");
        self.expect_kw("is", "after the entity name");
        loop {
            if self.at_kw("generic") {
                self.parse_generic_clause();
            } else if self.at_kw("port") {
                self.parse_port_clause();
            } else if self.at_kw("begin") || self.at_kw("end") || self.peek().kind == TokenKind::Eof
            {
                break;
            } else {
                self.parse_decl();
            }
        }
        if self.eat_kw("begin") {
            while !self.at_kw("end") && self.peek().kind != TokenKind::Eof {
                self.parse_concurrent_stmt();
            }
        }
        self.expect_kw("end", "to close the entity");
        self.eat_kw("entity");
        if self.peek().kind == TokenKind::Ident {
            self.bump();
        }
        self.expect_punct(';', "to end the entity");
        self.builder.finish_node();
    }

    pub(super) fn parse_architecture(&mut self) {
        self.start(SyntaxKind::Architecture);
        self.bump(); // architecture
        self.expect_ident("for the architecture name");
        self.expect_kw("of", "after the architecture name");
        self.expect_ident("for the entity name");
        self.expect_kw("is", "before the declarative part");
        while !self.at_kw("begin") && self.peek().kind != TokenKind::Eof {
            self.parse_decl();
        }
        self.expect_kw("begin", "before the concurrent statements");
        while !self.at_kw("end") && self.peek().kind != TokenKind::Eof {
            self.parse_concurrent_stmt();
        }
        self.expect_kw("end", "to close the architecture");
        self.eat_kw("architecture"); // `end [architecture] [name];`
        if self.peek().kind == TokenKind::Ident {
            self.bump(); // optional architecture name
        }
        self.expect_punct(';', "to end the architecture");
        self.builder.finish_node();
    }

    pub(super) fn parse_generic_clause(&mut self) {
        self.start(SyntaxKind::GenericClause);
        self.bump(); // generic
        self.expect_punct('(', "to open the generic list");
        while !self.at_punct(')') && self.peek().kind != TokenKind::Eof {
            // VHDL-2008 generic clauses may also carry `type T;` entries and
            // subprogram specifications (`impure function f(...) ...;`).
            if self.at_kw("function")
                || self.at_kw("procedure")
                || self.at_kw("pure")
                || self.at_kw("impure")
            {
                self.parse_subprogram(); // consumes its own ';'
                continue;
            }
            // VHDL-2008 generic package as a generic:
            // `package local_pkg is new g generic map (<>);`
            if self.at_kw("package")
                && self.peek2().kind == TokenKind::Ident
                && self.peek3_text().eq_ignore_ascii_case("is")
            {
                self.parse_package_inst(); // consumes its own ';'
                continue;
            }
            self.parse_interface_decl(SyntaxKind::GenericDecl);
            if !self.eat_punct(';') {
                break;
            }
        }
        self.expect_punct(')', "to close the generic list");
        // The `;` is optional here: a subprogram generic clause is followed
        // by `parameter (...)`, not by `;` (`function f generic (...) parameter ...`).
        self.eat_punct(';');
        self.builder.finish_node();
    }

    pub(super) fn parse_port_clause(&mut self) {
        self.start(SyntaxKind::PortClause);
        self.bump(); // port
        self.expect_punct('(', "to open the port list");
        while !self.at_punct(')') && self.peek().kind != TokenKind::Eof {
            self.parse_interface_decl(SyntaxKind::PortDecl);
            if !self.eat_punct(';') {
                break;
            }
        }
        self.expect_punct(')', "to close the port list");
        self.expect_punct(';', "after the port clause");
        self.builder.finish_node();
    }

    /// Component configuration specification:
    /// `for lbl {, lbl} | all : comp use entity|configuration ...;`
    pub(super) fn parse_for_spec(&mut self) {
        self.start(SyntaxKind::ForSpec);
        self.bump(); // for
        loop {
            if self.at_kw("all") {
                self.bump();
            } else if self.peek().kind == TokenKind::Ident {
                self.parse_name();
            } else {
                self.error_here("expected a label or `all` in the for-specification");
                break;
            }
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct(':', "in the for-specification");
        self.parse_name(); // component name
        self.expect_kw("use", "in the for-specification");
        if self.at_kw("entity") || self.at_kw("configuration") {
            self.bump();
        }
        self.parse_name();
        if self.eat_punct('(') {
            if self.peek().kind == TokenKind::Ident {
                self.bump(); // architecture name
            }
            self.expect_punct(')', "to close the architecture selection");
        }
        if self.at_kw("generic") {
            self.bump();
            self.expect_kw("map", "after generic in a configuration specification");
            self.parse_assoc_list();
        }
        if self.at_kw("port") {
            self.bump();
            self.expect_kw("map", "after port in a configuration specification");
            self.parse_assoc_list();
        }
        self.expect_punct(';', "to end the for-specification");
        self.builder.finish_node();
    }

    pub(super) fn parse_package(&mut self) {
        self.start(SyntaxKind::Package);
        self.bump(); // package
        self.eat_kw("body"); // package body: same node, body flag is in the text
        self.expect_ident("for the package name");
        self.expect_kw("is", "before the package declarative part");
        while !self.at_kw("end") && !self.at_kw("begin") && self.peek().kind != TokenKind::Eof {
            // VHDL-2008 generic packages: `generic (W : natural := 8);`
            if self.at_kw("generic") {
                self.parse_generic_clause();
            } else {
                self.parse_decl();
            }
        }
        if self.eat_kw("begin") {
            while !self.at_kw("end") && self.peek().kind != TokenKind::Eof {
                self.parse_seq_stmt();
            }
        }
        self.expect_kw("end", "to close the package");
        self.eat_kw("package"); // `end package [name];`
        self.eat_kw("body");
        if self.peek().kind == TokenKind::Ident {
            self.bump(); // optional package name
        }
        self.expect_punct(';', "to end the package");
        self.builder.finish_node();
    }
}
