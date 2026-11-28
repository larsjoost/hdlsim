//! Declarations: the declarative-item dispatch ([`Parser::parse_decl`]),
//! interface-facing object declarations (signal/constant/variable/file),
//! VHDL-2008 package instantiations, aliases (with subprogram signatures),
//! VHDL-2019 view declarations, attribute declarations and specifications,
//! components, and subprograms (incl. bodies).
//!
//! Child module of `vhdl::parser`, the same pattern as `design.rs`.

use crate::kinds::SyntaxKind;
use crate::token::TokenKind;

impl super::Parser<'_> {
    /// One declarative item. Object, type, and subprogram declarations
    /// dispatch here; every other form lives in [`Parser::parse_decl_other`].
    pub(super) fn parse_decl(&mut self) {
        match () {
            _ if self.at_kw("signal") => self.parse_simple_decl(SyntaxKind::SignalDecl, "signal"),
            _ if self.at_kw("constant") => {
                self.parse_simple_decl(SyntaxKind::ConstantDecl, "constant")
            }
            _ if self.at_kw("variable") => {
                self.parse_simple_decl(SyntaxKind::VariableDecl, "variable")
            }
            _ if self.at_kw("shared") => self.parse_shared_variable_decl(),
            _ if self.at_kw("type") => self.parse_type_decl(),
            _ if self.at_kw("subtype") => self.parse_subtype_decl(),
            _ if self.at_kw("component") => self.parse_component_decl(),
            _ if self.at_kw("file") => self.parse_file_object_decl(),
            _ if self.at_kw("function") || self.at_kw("procedure") => self.parse_subprogram(),
            // VHDL-2008 `pure`/`impure` function prefixes (kept as tokens in
            // the subprogram node; semantics unchanged for simulation).
            _ if self.at_kw("pure") || self.at_kw("impure") => self.parse_subprogram(),
            _ => self.parse_decl_other(),
        }
    }

    /// Declarative items that are not object/type/subprogram declarations:
    /// aliases, views, attributes, groups, disconnects, configuration
    /// specifications, and the stray forms legal inside protected bodies.
    /// The keyword sets are disjoint from [`Parser::parse_decl`]'s, so the
    /// two-level dispatch sees exactly the same inputs as the old flat match.
    pub(super) fn parse_decl_other(&mut self) {
        match () {
            _ if self.at_kw("alias") => self.parse_alias(),
            // VHDL-2019 view declaration: `view name of subtype is ... end view;`.
            _ if self.at_kw("view")
                && self.peek2().kind == TokenKind::Ident
                && self.peek3_text().eq_ignore_ascii_case("of") =>
            {
                self.parse_view_decl();
            }
            // `attribute a : type;` (decl) vs `attribute a of t : l is e;` (spec)
            _ if self.at_kw("attribute") => self.parse_attr_decl_or_spec(),
            _ if self.at_kw("group") => self.parse_group_decl(),
            _ if self.at_kw("disconnect") => self.parse_disconnect_spec(),
            _ if self.at_kw("use") => self.parse_use_clause(),
            _ if self.at_kw("for") => self.parse_for_spec(),
            // VHDL-2019 `private` section marker inside a protected-type body.
            _ if self.at_kw("private") => {
                self.bump();
            }
            // VHDL-2019 generic clause inside a protected-type body:
            // `type t is protected generic (type e_t;) ... end protected;`
            _ if self.at_kw("generic") && self.peek2_text() == "(" => {
                self.parse_generic_clause();
            }
            _ if self.at_kw("package")
                && self.peek2().kind == TokenKind::Ident
                && self.peek3_text().eq_ignore_ascii_case("is") =>
            {
                self.parse_package_inst();
            }
            _ => {
                self.error_here("expected a declaration");
                self.skip_to_semicolon();
            }
        }
    }

    /// `shared variable v : t;` — VariableDecl with the `shared` keyword kept
    /// in the tree.
    pub(super) fn parse_shared_variable_decl(&mut self) {
        self.start(SyntaxKind::VariableDecl);
        self.bump(); // shared
        self.expect_kw("variable", "after `shared`");
        self.parse_simple_decl_body();
        self.builder.finish_node();
    }

    /// `group name [is|: template] (...);` — parsed loosely, skipped.
    pub(super) fn parse_group_decl(&mut self) {
        self.bump(); // group
        if self.peek().kind == TokenKind::Ident {
            self.bump();
        }
        self.eat_kw("is");
        self.eat_punct(':');
        if self.peek().kind == TokenKind::Ident {
            self.bump();
        }
        self.skip_balanced('(', ')');
        self.expect_punct(';', "to end the group declaration");
    }

    /// `disconnect sig_list : type_mark [after expr];`
    pub(super) fn parse_disconnect_spec(&mut self) {
        self.start(SyntaxKind::DisconnectSpec);
        self.bump(); // disconnect
        loop {
            if self.peek().kind == TokenKind::Ident {
                self.parse_name();
            }
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct(':', "in the disconnect specification");
        self.parse_type_mark();
        if self.eat_kw("after") {
            self.parse_expression();
        }
        self.expect_punct(';', "to end the disconnect specification");
        self.builder.finish_node();
    }

    /// `attribute a : type;` (decl) vs `attribute a of t : l is e;` (spec).
    pub(super) fn parse_attr_decl_or_spec(&mut self) {
        if self.peek3_text() == ":" {
            self.parse_attr_decl();
        } else {
            self.parse_attr_spec();
        }
    }

    /// VHDL-2008 package instantiation: `package p is new g [generic map (...)];`
    pub(super) fn parse_package_inst(&mut self) {
        self.start(SyntaxKind::PackageInst);
        self.bump(); // package
        self.expect_ident("for the instantiated package name");
        self.expect_kw("is", "in the package instantiation");
        self.expect_kw("new", "in the package instantiation");
        self.parse_name(); // generic package name
        if self.at_kw("generic") {
            self.bump();
            self.expect_kw("map", "after generic");
            self.parse_assoc_list();
        }
        self.eat_punct(';'); // may be absent when nested in a generic clause
        self.builder.finish_node();
    }

    /// `alias name [(signature)] [: subtype] is target;`
    pub(super) fn parse_alias(&mut self) {
        self.start(SyntaxKind::AliasDecl);
        self.bump(); // alias
        self.parse_name();
        // Subprogram signature: `(param; ...)` — skip balanced.
        self.skip_balanced('(', ')');
        // Subprogram signature in brackets: `alias a is f[T1, T2];`
        self.skip_balanced('[', ']');
        if self.eat_punct(':') {
            self.parse_type_mark();
            if self.at_punct('(') {
                self.parse_constraint();
            }
        }
        self.expect_kw("is", "in the alias declaration");
        if self.at_punct2("<<") {
            // External-name alias: `alias x is << signal ^.dut.s : t >>;`.
            self.parse_ext_name();
        } else {
            self.parse_name();
        }
        // Subprogram signature after the name: `alias a is f[T, boolean return boolean];`
        self.skip_balanced('[', ']');
        // Optional signature on the target: `alias a is f[T1, T2];`
        self.skip_balanced('[', ']');
        self.expect_punct(';', "to end the alias declaration");
        self.builder.finish_node();
    }

    /// VHDL-2019 view declaration: `view name of subtype_indication is
    /// { view element | decl } end view [name];`. View elements are
    /// `name {, name} : [mode] [subtype_indication] ;` — the subtype may be
    /// omitted because the view's own subtype provides it.
    pub(super) fn parse_view_decl(&mut self) {
        self.start(SyntaxKind::ViewDecl);
        self.bump(); // view
        self.parse_name(); // view name
        self.expect_kw("of", "in the view declaration");
        self.parse_type_mark();
        while self.at_punct('(') {
            self.parse_constraint();
        }
        self.expect_kw("is", "in the view declaration");
        while !self.at_kw("end") && self.peek().kind != TokenKind::Eof {
            if self.at_kw("alias")
                || self.at_kw("attribute")
                || self.at_kw("use")
                || self.at_kw("shared")
            {
                self.parse_decl();
                continue;
            }
            self.parse_view_element();
        }
        self.expect_kw("end", "to close the view declaration");
        self.eat_kw("view");
        if self.peek().kind == TokenKind::Ident {
            self.bump(); // optional view name
        }
        self.expect_punct(';', "to end the view declaration");
        self.builder.finish_node();
    }

    /// One view element: `name {, name} : ([mode] subtype | view name) ;` —
    /// the subtype may be omitted (the view's own subtype provides it), and a
    /// nested `view` element refers to another view instead.
    pub(super) fn parse_view_element(&mut self) {
        self.start(SyntaxKind::PortDecl);
        loop {
            if self.peek().kind == TokenKind::Ident && !self.at_class_or_mode_kw() {
                self.parse_name();
            }
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct(':', "in the view element");
        if self.at_kw("view") {
            // Nested view element: `WriteAddress : view Axi4WriteAddressManagerView ;`
            self.bump();
            self.parse_name();
            if self.eat_kw("of") {
                self.parse_type_mark();
                while self.at_punct('(') {
                    self.parse_constraint();
                }
            }
        } else {
            if self.at_class_or_mode_kw() {
                self.bump(); // mode: in/out/inout
            }
            if !self.at_punct(';') {
                self.parse_type_mark();
                while self.at_punct('(') {
                    self.parse_constraint();
                }
            }
        }
        self.expect_punct(';', "to end the view element");
        self.builder.finish_node();
    }

    /// `attribute name : type;`
    pub(super) fn parse_attr_decl(&mut self) {
        self.start(SyntaxKind::AttrDecl);
        self.bump(); // attribute
        self.expect_ident("for the attribute name");
        self.expect_punct(':', "in the attribute declaration");
        self.parse_type_mark();
        self.expect_punct(';', "to end the attribute declaration");
        self.builder.finish_node();
    }

    /// `attribute name of target {, target} : label is expression;`
    pub(super) fn parse_attr_spec(&mut self) {
        self.start(SyntaxKind::AttrSpec);
        self.bump(); // attribute
        self.expect_ident("for the attribute name");
        self.expect_kw("of", "in the attribute specification");
        loop {
            self.parse_name();
            // Subprogram signature in the target: `f[ string return T ]`.
            if self.at_punct('[') {
                self.bump();
                let mut depth = 1;
                while depth > 0 && self.peek().kind != TokenKind::Eof {
                    if self.at_punct('[') {
                        depth += 1;
                    } else if self.at_punct(']') {
                        depth -= 1;
                    }
                    self.bump();
                }
            }
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct(':', "before the spec label");
        self.expect_ident("for the specification label");
        self.expect_kw("is", "in the attribute specification");
        if self.at_punct('(') {
            // Aggregate value, e.g. a physical position `( 540 um, 1200 um )`.
            self.bump();
            if !self.at_punct(')') {
                loop {
                    self.parse_expression();
                    if !self.eat_punct(',') {
                        break;
                    }
                }
            }
            self.expect_punct(')', "to close the aggregate attribute value");
        } else {
            self.parse_expression();
        }
        self.expect_punct(';', "to end the attribute specification");
        self.builder.finish_node();
    }

    /// `file f [, g] : type [open mode is expr];`
    pub(super) fn parse_file_object_decl(&mut self) {
        self.start(SyntaxKind::FileDecl);
        self.bump(); // file
        loop {
            if self.peek().kind == TokenKind::Ident {
                self.parse_name();
            }
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct(':', "before the file type");
        self.parse_type_mark();
        if self.eat_kw("is") {
            // `file f : t is "path";` — no `open` keyword.
            self.parse_expression();
        }
        if self.at_kw("open") {
            self.bump();
            if self.peek().kind == TokenKind::Ident {
                self.bump(); // read_mode / write_mode / append_mode
            }
            self.expect_kw("is", "in the file open clause");
            self.parse_expression();
        }
        self.expect_punct(';', "to end the file declaration");
        self.builder.finish_node();
    }

    /// `signal|constant|variable names : type [constraint] [:= init] ;`
    pub(super) fn parse_simple_decl(&mut self, kind: SyntaxKind, _kw: &str) {
        self.start(kind);
        self.bump(); // keyword
        self.parse_simple_decl_body();
        self.builder.finish_node();
    }

    /// Body of a simple decl with the node already open and the keyword consumed:
    /// `names : [bus|register] type [constraint] [:= init] ;`
    pub(super) fn parse_simple_decl_body(&mut self) {
        loop {
            if self.peek().kind == TokenKind::Ident {
                self.parse_name();
            }
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct(':', "before the type mark");
        // Signal kind modifiers (VHDL-93): `signal s : register std_logic;`
        if self.at_kw("bus") || self.at_kw("register") {
            self.bump();
        }
        self.parse_type_mark();
        while self.at_punct('(') {
            self.parse_constraint();
        }
        // Port kind: `signal q : out resolved_byte bus)`.
        if self.at_kw("bus") || self.at_kw("register") {
            self.bump();
        }
        if self.at_kw("range") {
            self.start(SyntaxKind::Range);
            self.bump();
            self.parse_range();
            self.builder.finish_node();
        }
        // Trailing signal-kind / resolution words: `signal s : pulled_up bit bus;`
        // (resolution function + type mark + kind) — skip stray idents.
        while self.peek().kind == TokenKind::Ident && self.peek_op2().as_deref() != Some(":=") {
            self.bump();
        }
        if self.peek_op2().as_deref() == Some(":=") {
            self.bump();
            self.bump();
            self.parse_expression();
        }
        self.expect_punct(';', "to end the declaration");
    }

    pub(super) fn parse_component_decl(&mut self) {
        self.start(SyntaxKind::ComponentDecl);
        self.bump(); // component
        self.expect_ident("for the component name");
        self.eat_kw("is"); // `is` is optional
        loop {
            if self.at_kw("generic") {
                self.parse_generic_clause();
            } else if self.at_kw("port") {
                self.parse_port_clause();
            } else {
                break;
            }
        }
        self.expect_kw("end", "to close the component");
        self.eat_kw("component");
        if self.peek().kind == TokenKind::Ident {
            self.bump();
        }
        self.expect_punct(';', "to end the component declaration");
        self.builder.finish_node();
    }

    /// Function/procedure declaration and (optional) body. Bodies are parsed
    /// into the tree; lowering them is a later milestone.
    pub(super) fn parse_subprogram(&mut self) {
        self.start(SyntaxKind::Function);
        // Optional VHDL-2008 purity prefix: `impure function f ...`.
        if self.at_kw("pure") || self.at_kw("impure") {
            self.bump();
        }
        self.bump(); // function | procedure
        self.parse_subprogram_name();
        self.parse_subprogram_params();
        if self.eat_kw("is") {
            self.parse_subprogram_body();
        }
        // Tolerate a missing ';' right before the ')' of a generic list
        // (some sources rely on it).
        if !self.eat_punct(';') && !self.at_punct(')') {
            self.error_here("expected ';' to end the subprogram");
        }
        self.builder.finish_node();
    }

    /// The subprogram name: an identifier or an operator symbol string
    /// literal (`function "and" (...)`).
    pub(super) fn parse_subprogram_name(&mut self) {
        if self.peek().kind == TokenKind::Ident || self.peek().kind == TokenKind::StrLiteral {
            self.builder.start_node(SyntaxKind::Name);
            self.bump();
            self.builder.finish_node();
        } else {
            self.error_here("expected a subprogram name");
        }
    }

    /// Header parameters: the VHDL-2019 subprogram generic clause
    /// (`function f generic (type T) parameter (...) return ...;`), the
    /// VHDL-2008 `parameter (...)` interface list, and the `return` type mark
    /// (`return rv_t of std_logic_vector` refines it with an `of` clause).
    pub(super) fn parse_subprogram_params(&mut self) {
        if self.at_kw("generic") && self.peek2_text() == "(" {
            self.parse_generic_clause();
        }
        self.eat_kw("parameter");
        if self.eat_punct('(') {
            while !self.at_punct(')') && self.peek().kind != TokenKind::Eof {
                self.parse_interface_decl(SyntaxKind::PortDecl);
                if !self.eat_punct(';') {
                    break;
                }
            }
            self.expect_punct(')', "to close the parameter list");
        }
        if self.at_kw("return") {
            self.bump();
            self.parse_type_mark();
            if self.eat_kw("of") {
                self.parse_type_mark();
            }
        }
    }

    /// The body after `is`: declarations, `begin`, sequential statements, and
    /// the trailing `end [function|procedure] [name]`.
    pub(super) fn parse_subprogram_body(&mut self) {
        while !self.at_kw("begin") && self.peek().kind != TokenKind::Eof {
            self.parse_decl();
        }
        self.eat_kw("begin");
        while !self.at_kw("end") && self.peek().kind != TokenKind::Eof {
            self.parse_seq_stmt();
        }
        self.expect_kw("end", "to close the subprogram body");
        if self.peek().kind == TokenKind::Ident {
            self.bump(); // function/procedure
        }
        // Optional name — an operator symbol is a string literal: `end function "ABS";`
        if self.peek().kind == TokenKind::Ident || self.peek().kind == TokenKind::StrLiteral {
            self.bump(); // name
        }
    }
}
