//! VHDL parser: tokens → lossless CST (rowan-style via [`TreeBuilder`]).
//!
//! One function per grammar production, mirroring IEEE 1076 clause names.
//! Covers the practical subset: library/use clauses, entities, architectures,
//! packages (declarations; subprogram bodies are parsed but lowered later),
//! processes, concurrent/sequential statements, and the full expression
//! precedence chain. VHDL is case-insensitive: keyword checks lowercase the
//! token text; the CST keeps the original text (lossless).
//!
//! Recovery: unknown constructs produce an `ErrorToken` + diagnostic and the
//! parser skips to the next `;` or a known keyword, so one bad unit does not
//! kill the rest of the file.

use crate::cst::{SyntaxNode, TreeBuilder};
use crate::diagnostic::{Diagnostic, Severity};
use crate::kinds::SyntaxKind;
use crate::source::{FileId, SourceFile};
use crate::token::{Token, TokenKind};

/// Output of parsing one VHDL file.
pub struct ParsedFile {
    pub root: SyntaxNode,
    pub diags: Vec<Diagnostic>,
}

/// Lex + parse one file.
pub fn parse_file(file: FileId, src: &SourceFile) -> ParsedFile {
    let (tokens, lex_diags) = super::lexer::lex(file, src);
    let mut p = Parser {
        src,
        tokens: &tokens,
        pos: 0,
        builder: TreeBuilder::new(file),
        diags: lex_diags,
    };
    p.parse_design_file();
    let root = p.builder.finish();
    ParsedFile {
        root,
        diags: p.diags,
    }
}

struct Parser<'a> {
    src: &'a SourceFile,
    tokens: &'a [Token],
    pos: usize,
    builder: TreeBuilder,
    diags: Vec<Diagnostic>,
}

/// Keyword operators, by precedence level (see parse_binops).
const LOGICAL_OPS: &[&str] = &["and", "or", "nand", "nor", "xor", "xnor"];
const SHIFT_OPS: &[&str] = &["sll", "srl", "sla", "sra", "rol", "ror"];
const ADD_KW_OPS: &[&str] = &["mod", "rem"]; // `+`, `-`, `&` are punct
const MUL_KW_OPS: &[&str] = &[]; // `*`, `/` are punct; mod/rem live at add level per LRM

/// (precedence, right_assoc) for a binary operator token/keyword.
fn binop_prec(op: &str) -> Option<(u8, bool)> {
    Some(match op {
        "and" | "or" | "nand" | "nor" | "xor" | "xnor" => (1, false),
        "=" | "/=" | "<" | "<=" | ">" | ">=" => (2, false),
        "sll" | "srl" | "sla" | "sra" | "rol" | "ror" => (3, false),
        "+" | "-" | "&" => (4, false),
        "*" | "/" | "mod" | "rem" => (5, false),
        "**" => (7, true),
        _ => return None,
    })
}

impl<'a> Parser<'a> {
    // ---- token navigation ---------------------------------------------------

    fn skip_trivia_from(&self, mut i: usize) -> usize {
        while i < self.tokens.len()
            && matches!(
                self.tokens[i].kind,
                TokenKind::Whitespace | TokenKind::Comment
            )
        {
            i += 1;
        }
        i
    }

    fn peek(&self) -> &Token {
        &self.tokens[self.skip_trivia_from(self.pos)]
    }

    fn peek2(&self) -> &Token {
        let i = self.skip_trivia_from(self.pos);
        let j = self.skip_trivia_from(i + 1);
        &self.tokens[j.min(self.tokens.len() - 1)]
    }

    fn peek_text(&self) -> &str {
        let t = self.peek();
        &self.src.text()[t.span.start as usize..t.span.end as usize]
    }

    fn peek2_text(&self) -> &str {
        let t = self.peek2();
        &self.src.text()[t.span.start as usize..t.span.end as usize]
    }

    fn peek3_text(&self) -> &str {
        let i = self.skip_trivia_from(self.pos);
        let j = self.skip_trivia_from(i + 1);
        let k = self.skip_trivia_from(j + 1);
        let t = &self.tokens[k.min(self.tokens.len() - 1)];
        &self.src.text()[t.span.start as usize..t.span.end as usize]
    }

    /// `ident :` starts a label — but `ident :=` is an assignment, so the
    /// colon must not be followed by `=`.
    fn at_label(&self) -> bool {
        self.peek().kind == TokenKind::Ident && self.peek2_text() == ":" && self.peek3_text() != "="
    }

    fn at_kw(&self, kw: &str) -> bool {
        let t = self.peek();
        t.kind == TokenKind::Ident && self.peek_text().eq_ignore_ascii_case(kw)
    }

    fn at_punct(&self, c: char) -> bool {
        let t = self.peek();
        t.kind == TokenKind::Punct && t.span.len() == 1 && self.peek_text() == c.to_string()
    }

    /// Two adjacent punct tokens forming a two-char operator (`<=`, `:=`, `=>`, `/=`, `**`, `<>`).
    fn peek_op2(&self) -> Option<String> {
        let i0 = self.skip_trivia_from(self.pos);
        let i1 = self.skip_trivia_from(i0 + 1);
        if i0 >= self.tokens.len() || i1 >= self.tokens.len() {
            return None;
        }
        let (a, b) = (&self.tokens[i0], &self.tokens[i1]);
        if a.kind != TokenKind::Punct || b.kind != TokenKind::Punct {
            return None;
        }
        let t = format!(
            "{}{}",
            &self.src.text()[a.span.start as usize..a.span.end as usize],
            &self.src.text()[b.span.start as usize..b.span.end as usize]
        );
        match t.as_str() {
            "<=" | ":=" | "=>" | "/=" | "**" | "<>" | ">=" => Some(t),
            _ => None,
        }
    }

    /// Emit pending trivia tokens into the currently open node and advance
    /// past them, so a node opened next starts at a real token.
    fn bump_trivia(&mut self) {
        while self.pos < self.tokens.len() {
            let t = &self.tokens[self.pos];
            if matches!(t.kind, TokenKind::Whitespace | TokenKind::Comment) {
                self.builder
                    .token(SyntaxKind::from_token(t.kind), t.span.len());
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    /// Open a node at the next real token (skipping trivia first, so trivia
    /// never becomes a child of the new node).
    fn start(&mut self, kind: SyntaxKind) {
        self.bump_trivia();
        self.builder.start_node(kind);
    }

    fn bump(&mut self) -> Token {
        self.bump_trivia();
        let t = self.tokens[self.pos].clone();
        self.builder
            .token(SyntaxKind::from_token(t.kind), t.span.len());
        self.pos += 1;
        t
    }

    fn eat_punct(&mut self, c: char) -> bool {
        if self.at_punct(c) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn eat_kw(&mut self, kw: &str) -> bool {
        if self.at_kw(kw) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect_punct(&mut self, c: char, ctx: &str) -> bool {
        if self.eat_punct(c) {
            true
        } else {
            self.error_here(&format!("expected '{}' {}", c, ctx));
            false
        }
    }

    fn expect_kw(&mut self, kw: &str, ctx: &str) -> bool {
        if self.eat_kw(kw) {
            true
        } else {
            self.error_here(&format!("expected '{}' {}", kw, ctx));
            false
        }
    }

    fn expect_ident(&mut self, ctx: &str) -> Option<String> {
        if self.peek().kind == TokenKind::Ident {
            let t = self.peek_text().to_string();
            self.bump();
            Some(t)
        } else {
            self.error_here(&format!("expected an identifier {}", ctx));
            None
        }
    }

    fn error_here(&mut self, msg: &str) {
        let t = self.peek().clone();
        self.diags.push(Diagnostic {
            code: "E0301",
            severity: Severity::Error,
            span: Some(t.span),
            message: msg.to_string(),
            help: None,
        });
    }

    // ---- design file ----------------------------------------------------------

    fn parse_design_file(&mut self) {
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
                self.parse_package();
            } else if self.at_kw("configuration") {
                self.error_here("configuration declarations are not supported yet");
                self.skip_to_semicolon();
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

    fn skip_to_semicolon(&mut self) {
        while self.peek().kind != TokenKind::Eof && !self.at_punct(';') {
            self.bump();
        }
        self.eat_punct(';');
    }

    fn parse_library_clause(&mut self) {
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

    fn parse_use_clause(&mut self) {
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

    // ---- entity / architecture -------------------------------------------------

    fn parse_entity(&mut self) {
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

    fn parse_architecture(&mut self) {
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
        if self.peek().kind == TokenKind::Ident {
            self.bump(); // architecture name
        }
        self.expect_punct(';', "to end the architecture");
        self.builder.finish_node();
    }

    fn parse_generic_clause(&mut self) {
        self.start(SyntaxKind::GenericClause);
        self.bump(); // generic
        self.expect_punct('(', "to open the generic list");
        while !self.at_punct(')') && self.peek().kind != TokenKind::Eof {
            self.parse_interface_decl(SyntaxKind::GenericDecl);
            if !self.eat_punct(';') {
                break;
            }
        }
        self.expect_punct(')', "to close the generic list");
        self.expect_punct(';', "after the generic clause");
        self.builder.finish_node();
    }

    fn parse_port_clause(&mut self) {
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

    /// `name [, name...] : [mode] type_mark [constraint] [:= default]`.
    fn parse_interface_decl(&mut self, kind: SyntaxKind) {
        self.start(kind);
        loop {
            if self.peek().kind == TokenKind::Ident {
                self.parse_name();
            }
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct(':', "before the mode/type");
        // Optional mode: in/out/inout/buffer/linkage (an identifier).
        if self.at_kw("in")
            || self.at_kw("out")
            || self.at_kw("inout")
            || self.at_kw("buffer")
            || self.at_kw("linkage")
        {
            self.bump();
        }
        self.parse_type_mark();
        if self.at_punct('(') {
            self.parse_constraint();
        }
        if self.peek_op2().as_deref() == Some(":=") {
            self.bump();
            self.bump();
            self.parse_expression();
        }
        self.builder.finish_node();
    }

    // ---- declarations ------------------------------------------------------------

    fn parse_decl(&mut self) {
        match () {
            _ if self.at_kw("signal") => self.parse_simple_decl(SyntaxKind::SignalDecl, "signal"),
            _ if self.at_kw("constant") => {
                self.parse_simple_decl(SyntaxKind::ConstantDecl, "constant")
            }
            _ if self.at_kw("variable") => {
                self.parse_simple_decl(SyntaxKind::VariableDecl, "variable")
            }
            _ if self.at_kw("type") => self.parse_type_decl(),
            _ if self.at_kw("subtype") => self.parse_subtype_decl(),
            _ if self.at_kw("component") => self.parse_component_decl(),
            _ if self.at_kw("function") || self.at_kw("procedure") => self.parse_subprogram(),
            _ if self.at_kw("attribute")
                || self.at_kw("alias")
                || self.at_kw("file")
                || self.at_kw("shared")
                || self.at_kw("use")
                || self.at_kw("for") =>
            {
                // `use`/`for` (attribute specs) etc.: skip to ';'.
                self.skip_to_semicolon();
            }
            _ => {
                self.error_here("expected a declaration");
                self.skip_to_semicolon();
            }
        }
    }

    /// `signal|constant|variable names : type [constraint] [:= init] ;`
    fn parse_simple_decl(&mut self, kind: SyntaxKind, _kw: &str) {
        self.start(kind);
        self.bump(); // keyword
        loop {
            if self.peek().kind == TokenKind::Ident {
                self.parse_name();
            }
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct(':', "before the type mark");
        self.parse_type_mark();
        if self.at_punct('(') {
            self.parse_constraint();
        }
        if self.peek_op2().as_deref() == Some(":=") {
            self.bump();
            self.bump();
            self.parse_expression();
        }
        self.expect_punct(';', "to end the declaration");
        self.builder.finish_node();
    }

    fn parse_type_decl(&mut self) {
        self.start(SyntaxKind::TypeDecl);
        self.bump(); // type
        self.expect_ident("for the type name");
        self.expect_kw("is", "after the type name");
        if self.at_punct('(') {
            // Enumeration type: (idle, running, ...)
            self.bump();
            loop {
                if self.peek().kind == TokenKind::Ident
                    || self.peek().kind == TokenKind::CharLiteral
                {
                    self.bump();
                } else {
                    self.error_here("expected an enumeration literal");
                    break;
                }
                if !self.eat_punct(',') {
                    break;
                }
            }
            self.expect_punct(')', "to close the enumeration");
        } else if self.at_kw("range") {
            self.bump();
            self.parse_range();
        } else if self.at_kw("array") {
            // array (range) of type — parse loosely.
            self.bump();
            if self.at_punct('(') {
                self.parse_constraint();
            }
            self.expect_kw("of", "in the array type");
            self.parse_type_mark();
            if self.at_punct('(') {
                // Constrained element type: `... of std_logic_vector(31 downto 0)`.
                self.parse_constraint();
            }
        } else if self.at_kw("record") {
            self.skip_balanced_until_end("record");
        } else {
            self.error_here("unsupported type definition (only enum/range/array/record-skip)");
            self.skip_to_semicolon();
        }
        self.expect_punct(';', "to end the type declaration");
        self.builder.finish_node();
    }

    fn parse_subtype_decl(&mut self) {
        self.start(SyntaxKind::SubtypeDecl);
        self.bump(); // subtype
        self.expect_ident("for the subtype name");
        self.expect_kw("is", "after the subtype name");
        self.parse_type_mark();
        if self.at_punct('(') {
            self.parse_constraint();
        } else if self.at_kw("range") {
            self.bump();
            self.parse_range();
        }
        self.expect_punct(';', "to end the subtype declaration");
        self.builder.finish_node();
    }

    fn parse_component_decl(&mut self) {
        self.start(SyntaxKind::ComponentDecl);
        self.bump(); // component
        self.expect_ident("for the component name");
        if self.eat_kw("is") {
            loop {
                if self.at_kw("generic") {
                    self.parse_generic_clause();
                } else if self.at_kw("port") {
                    self.parse_port_clause();
                } else {
                    break;
                }
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
    fn parse_subprogram(&mut self) {
        self.start(SyntaxKind::Function);
        self.bump(); // function | procedure
        self.expect_ident("for the subprogram name");
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
        }
        if self.eat_kw("is") {
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
            if self.peek().kind == TokenKind::Ident {
                self.bump(); // name
            }
        }
        self.expect_punct(';', "to end the subprogram");
        self.builder.finish_node();
    }

    fn skip_balanced_until_end(&mut self, kw: &str) {
        // Skip a record body etc.: consume until `end <kw>`.
        while self.peek().kind != TokenKind::Eof {
            if self.at_kw("end") {
                self.bump();
                self.eat_kw(kw);
                return;
            }
            self.bump();
        }
    }

    /// A type mark: a (possibly selected) name, e.g. `std_logic_vector`.
    /// No paren suffixes — a following `(7 downto 0)` is a constraint, parsed
    /// separately by the caller.
    fn parse_type_mark(&mut self) {
        if self.peek().kind == TokenKind::Ident {
            self.start(SyntaxKind::Name);
            self.bump();
            while self.at_punct('.') {
                self.bump();
                if self.peek().kind == TokenKind::Ident {
                    self.bump();
                } else {
                    self.error_here("expected a name after '.'");
                    break;
                }
            }
            self.builder.finish_node();
        } else {
            self.error_here("expected a type mark");
            self.bump();
        }
    }

    /// `(expr to|downto expr)` / `(expr)` / `range ...` — an index/range constraint.
    fn parse_constraint(&mut self) {
        self.start(SyntaxKind::Range);
        self.expect_punct('(', "to open the constraint");
        self.parse_range();
        self.expect_punct(')', "to close the constraint");
        self.builder.finish_node();
    }

    fn parse_range(&mut self) {
        self.parse_expression();
        if self.at_kw("to") || self.at_kw("downto") {
            self.bump();
            self.parse_expression();
        }
    }

    // ---- packages ----------------------------------------------------------------

    fn parse_package(&mut self) {
        self.start(SyntaxKind::Package);
        self.bump(); // package
        self.eat_kw("body"); // package body: same node, body flag is in the text
        self.expect_ident("for the package name");
        self.expect_kw("is", "before the package declarative part");
        while !self.at_kw("end") && !self.at_kw("begin") && self.peek().kind != TokenKind::Eof {
            self.parse_decl();
        }
        if self.eat_kw("begin") {
            while !self.at_kw("end") && self.peek().kind != TokenKind::Eof {
                self.parse_seq_stmt();
            }
        }
        self.expect_kw("end", "to close the package");
        if self.peek().kind == TokenKind::Ident {
            self.bump(); // optional package name
        }
        self.expect_punct(';', "to end the package");
        self.builder.finish_node();
    }

    // ---- concurrent statements -----------------------------------------------------

    fn parse_concurrent_stmt(&mut self) {
        // Optional label: `ident :`
        if self.at_label() {
            self.bump(); // label
            self.bump(); // ':'
        }
        if self.at_kw("process") {
            self.parse_process();
        } else if self.at_kw("assert") {
            self.parse_assert(true);
        } else if self.at_kw("block") || self.at_kw("if") || self.at_kw("for") || self.at_kw("case")
        {
            self.error_here("generate blocks are not supported yet");
            self.skip_balanced_until_end("block");
            self.eat_punct(';');
        } else if self.at_kw("entity")
            || self.at_kw("component")
            || (self.peek().kind == TokenKind::Ident && self.peek2().kind == TokenKind::Ident)
        {
            self.parse_instantiation();
        } else {
            // Concurrent signal assignment: target <= waveform ;
            self.start(SyntaxKind::ConcAssign);
            self.parse_name();
            if self.peek_op2().as_deref() == Some("<=") {
                self.bump();
                self.bump();
            } else {
                self.error_here("expected '<=' in a concurrent signal assignment");
            }
            self.parse_expression();
            // waveform may continue: `, expr after ...` / `after expr` — skip extras.
            while self.eat_punct(',') {
                self.parse_expression();
            }
            if self.at_kw("after") {
                self.bump();
                self.parse_expression();
            }
            self.expect_punct(';', "to end the concurrent assignment");
            self.builder.finish_node();
        }
    }

    fn parse_process(&mut self) {
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
        self.eat_kw("process");
        if self.peek().kind == TokenKind::Ident {
            self.bump(); // optional label
        }
        self.expect_punct(';', "to end the process");
        self.builder.finish_node();
    }

    /// `label : comp [generic map (...)] port map (...);` or `entity lib.name(arch) ...`.
    fn parse_instantiation(&mut self) {
        self.start(SyntaxKind::Inst);
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

    /// `( assoc {, assoc} )` where assoc is `.name => expr`, `name => expr`,
    /// or a positional expression.
    fn parse_assoc_list(&mut self) {
        self.expect_punct('(', "to open the association list");
        while !self.at_punct(')') && self.peek().kind != TokenKind::Eof {
            self.start(SyntaxKind::PortConn);
            let named = self.at_punct('.')
                || (self.peek().kind == TokenKind::Ident
                    && self.peek2_text() == "="
                    && self.peek3_text() == ">");
            if named {
                if self.eat_punct('.') {
                    let m = self.builder.mark();
                    if self.peek().kind == TokenKind::Ident {
                        self.bump();
                    }
                    self.builder.start_node_at(m, SyntaxKind::Name);
                    self.builder.finish_node();
                } else {
                    let m = self.builder.mark();
                    self.bump(); // association name
                    self.builder.start_node_at(m, SyntaxKind::Name);
                    self.builder.finish_node();
                }
                if self.peek_op2().as_deref() == Some("=>") {
                    self.bump();
                    self.bump();
                } else {
                    self.error_here("expected '=>' in the association");
                }
            }
            if !self.at_punct(')') {
                self.parse_expression();
            }
            self.builder.finish_node();
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct(')', "to close the association list");
    }

    fn parse_assert(&mut self, _concurrent: bool) {
        self.start(SyntaxKind::AssertStmt);
        self.bump(); // assert
        self.parse_expression(); // condition
        if self.eat_kw("report") {
            self.parse_expression();
        }
        if self.eat_kw("severity") {
            if self.peek().kind == TokenKind::Ident {
                self.bump();
            }
        }
        self.expect_punct(';', "to end the assert statement");
        self.builder.finish_node();
    }

    // ---- sequential statements -------------------------------------------------------

    fn parse_seq_stmt(&mut self) {
        if self.at_label() {
            self.bump(); // label
            self.bump(); // ':'
        }
        if self.at_kw("if") {
            self.parse_if();
        } else if self.at_kw("case") {
            self.parse_case();
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
            self.start(SyntaxKind::AssertStmt);
            self.bump();
            self.parse_expression();
            if self.eat_kw("severity") && self.peek().kind == TokenKind::Ident {
                self.bump();
            }
            self.expect_punct(';', "to end the report statement");
            self.builder.finish_node();
        } else if self.at_kw("exit") || self.at_kw("next") {
            self.start(SyntaxKind::ExitStmt);
            self.bump();
            if self.eat_kw("when") {
                self.parse_expression();
            }
            self.expect_punct(';', "to end the exit/next statement");
            self.builder.finish_node();
        } else if self.at_kw("return") {
            self.start(SyntaxKind::ReturnStmt);
            self.bump();
            if !self.at_punct(';') {
                self.parse_expression();
            }
            self.expect_punct(';', "to end the return statement");
            self.builder.finish_node();
        } else {
            // Assignment or procedure call: parse the target/name, then decide.
            let mark = self.builder.mark();
            self.parse_postfix_name();
            if self.peek_op2().as_deref() == Some("<=") {
                self.builder.start_node_at(mark, SyntaxKind::SeqAssign);
                self.bump();
                self.bump();
                self.parse_expression();
                if self.at_kw("after") {
                    self.bump();
                    self.parse_expression();
                }
                self.expect_punct(';', "to end the signal assignment");
                self.builder.finish_node();
            } else if self.peek_op2().as_deref() == Some(":=") {
                self.builder.start_node_at(mark, SyntaxKind::VarAssign);
                self.bump();
                self.bump();
                self.parse_expression();
                self.expect_punct(';', "to end the variable assignment");
                self.builder.finish_node();
            } else {
                self.builder.start_node_at(mark, SyntaxKind::CallStmt);
                self.expect_punct(';', "to end the statement");
                self.builder.finish_node();
            }
        }
    }

    fn parse_if(&mut self) {
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
    fn parse_seq_block(&mut self) {
        while !self.at_kw("elsif")
            && !self.at_kw("else")
            && !self.at_kw("end")
            && !self.at_kw("when")
            && self.peek().kind != TokenKind::Eof
        {
            self.parse_seq_stmt();
        }
    }

    fn parse_case(&mut self) {
        self.start(SyntaxKind::Case);
        self.bump(); // case
        self.parse_expression();
        self.expect_kw("is", "after the case expression");
        loop {
            if self.at_kw("end") || self.peek().kind == TokenKind::Eof {
                break;
            }
            self.expect_kw("when", "before a case alternative");
            self.start(SyntaxKind::CaseItem);
            if self.eat_kw("others") {
                self.start(SyntaxKind::Others);
                self.builder.finish_node();
            } else {
                loop {
                    self.parse_expression();
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
        self.expect_kw("end", "to close the case");
        self.eat_kw("case");
        if self.peek().kind == TokenKind::Ident {
            self.bump();
        }
        self.expect_punct(';', "to end the case statement");
        self.builder.finish_node();
    }

    /// `for i in range loop ... end loop;`
    fn parse_for_loop(&mut self) {
        self.start(SyntaxKind::For);
        self.bump(); // for
        self.expect_ident("for the loop variable");
        self.expect_kw("in", "after the loop variable");
        // Discrete range: `expr to/downto expr`, or `name'range`, or a subtype.
        if self.peek().kind == TokenKind::Ident
            && self.peek2().kind == TokenKind::Punct
            && self.peek2_text() == "'"
        {
            self.parse_name(); // includes the 'range attribute
        } else {
            self.start(SyntaxKind::Range);
            self.parse_range();
            self.builder.finish_node();
        }
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

    fn parse_loop(&mut self) {
        self.start(SyntaxKind::LoopStmt);
        if self.eat_kw("while") {
            self.parse_expression();
        } else {
            self.bump(); // loop
        }
        self.expect_kw("loop", "after the loop header");
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

    fn parse_wait(&mut self) {
        self.start(SyntaxKind::Wait);
        self.bump(); // wait
        if self.eat_kw("until") {
            self.parse_expression();
        }
        if self.eat_kw("on") {
            self.start(SyntaxKind::SensList);
            loop {
                self.parse_name();
                if !self.eat_punct(',') {
                    break;
                }
            }
            self.builder.finish_node();
        }
        if self.eat_kw("for") {
            self.parse_expression();
            // Physical literal unit: `10 ns` — the unit is a bare identifier.
            if self.peek().kind == TokenKind::Ident {
                self.bump();
            }
        }
        self.expect_punct(';', "to end the wait statement");
        self.builder.finish_node();
    }

    // ---- names and expressions -----------------------------------------------------

    /// A name: `ident` with selected (`.b`), indexed (`(i)`), sliced
    /// (`(msb downto lsb)`), called (`(args)`), or attributed (`'attr`) suffixes.
    fn parse_name(&mut self) {
        let mark = self.builder.mark();
        if self.peek().kind == TokenKind::Ident {
            self.start(SyntaxKind::Name);
            self.bump();
            while self.at_punct('.') {
                self.bump();
                if self.peek().kind == TokenKind::Ident {
                    self.bump();
                } else {
                    self.error_here("expected a name after '.'");
                    break;
                }
            }
            self.builder.finish_node();
        } else {
            self.error_here("expected a name");
            self.bump();
            return;
        }
        self.parse_name_suffixes(mark);
    }

    fn parse_postfix_name(&mut self) {
        self.parse_name();
    }

    fn parse_name_suffixes(&mut self, mark: crate::cst::Mark) {
        loop {
            if self.at_punct('(') {
                self.bump();
                self.parse_expression();
                if self.at_kw("to") || self.at_kw("downto") {
                    // Slice: `name(msb downto lsb)` — wrap name+range in PartSelect.
                    self.bump(); // to / downto
                    self.parse_expression();
                    self.expect_punct(')', "to close the slice");
                    self.builder.start_node_at(mark, SyntaxKind::PartSelect);
                    self.builder.finish_node();
                } else {
                    // Index or call args.
                    while self.eat_punct(',') {
                        self.parse_expression();
                    }
                    self.expect_punct(')', "to close the argument/index list");
                    self.builder.start_node_at(mark, SyntaxKind::Call);
                    self.builder.finish_node();
                }
            } else if self.at_punct('\'') {
                self.bump();
                if self.peek().kind == TokenKind::Ident {
                    self.builder.start_node_at(mark, SyntaxKind::Attr);
                    self.bump(); // attribute name
                    self.builder.finish_node();
                } else {
                    self.error_here("expected an attribute name after the tick");
                }
            } else {
                break;
            }
        }
    }

    pub fn parse_expression(&mut self) {
        self.parse_expr_prec(1);
    }

    fn parse_expr_prec(&mut self, min_prec: u8) {
        let mark = self.builder.mark();
        self.parse_unary();
        self.parse_binops(mark, min_prec);
    }

    fn current_binop(&self) -> Option<String> {
        if let Some(op) = self.peek_op2() {
            // A two-char punct sequence that is not a binary operator (e.g.
            // `=>`, `:=`) terminates the expression.
            return binop_prec(&op).map(|_| op);
        }
        let t = self.peek();
        if t.kind == TokenKind::Ident {
            let kw = self.peek_text();
            if LOGICAL_OPS.contains(&kw.to_ascii_lowercase().as_str())
                || SHIFT_OPS.contains(&kw.to_ascii_lowercase().as_str())
                || ADD_KW_OPS.contains(&kw.to_ascii_lowercase().as_str())
                || MUL_KW_OPS.contains(&kw.to_ascii_lowercase().as_str())
            {
                return Some(kw.to_ascii_lowercase());
            }
            return None;
        }
        if t.kind == TokenKind::Punct {
            let c = self.peek_text();
            if matches!(c, "=" | "<" | ">" | "+" | "-" | "*" | "/" | "&") && binop_prec(c).is_some()
            {
                return Some(c.to_string());
            }
        }
        None
    }

    fn parse_binops(&mut self, mark: crate::cst::Mark, min_prec: u8) {
        loop {
            let Some(op) = self.current_binop() else {
                break;
            };
            let Some((prec, right_assoc)) = binop_prec(&op) else {
                break;
            };
            if prec < min_prec {
                break;
            }
            self.builder.start_node_at(mark, SyntaxKind::BinExpr);
            // Consume the operator: one punct, two puncts (<=, **, /=), or a keyword.
            if op.len() == 2 && self.peek().kind == TokenKind::Punct {
                self.bump();
                self.bump();
            } else {
                self.bump();
            }
            let next_min = if right_assoc { prec } else { prec + 1 };
            self.parse_expr_prec(next_min);
            self.builder.finish_node();
            // Keep `mark`: the next operator in the chain wraps the whole
            // BinExpr just finished (left-associative).
        }
    }

    fn parse_unary(&mut self) {
        if self.at_punct('-') || self.at_punct('+') {
            self.start(SyntaxKind::UnaryExpr);
            self.bump();
            self.parse_unary();
            self.builder.finish_node();
        } else if self.at_kw("not") || self.at_kw("abs") {
            self.start(SyntaxKind::UnaryExpr);
            self.bump();
            self.parse_unary();
            self.builder.finish_node();
        } else {
            self.parse_primary();
        }
    }

    fn parse_primary(&mut self) {
        let t = self.peek().clone();
        match t.kind {
            TokenKind::IntLiteral
            | TokenKind::RealLiteral
            | TokenKind::StrLiteral
            | TokenKind::CharLiteral => {
                self.start(SyntaxKind::Literal);
                self.bump();
                self.builder.finish_node();
            }
            TokenKind::Ident if self.peek_text().eq_ignore_ascii_case("others") => {
                self.start(SyntaxKind::Others);
                self.bump();
                self.builder.finish_node();
            }
            TokenKind::Ident
                if matches!(
                    self.peek_text().to_ascii_lowercase().as_str(),
                    "x" | "b" | "o"
                ) && self.peek2().kind == TokenKind::StrLiteral =>
            {
                // Bit string literal: x"FF" / b"01" / o"17".
                self.start(SyntaxKind::Literal);
                self.bump();
                self.bump();
                self.builder.finish_node();
            }
            TokenKind::Ident => {
                self.parse_name();
            }
            TokenKind::Punct if self.peek_text() == "(" => {
                // Paren expr or aggregate `(a, b, others => x)`.
                // The mark goes before `(` so the paren is inside the node.
                let mark = self.builder.mark();
                self.bump();
                if self.at_punct(')') {
                    // Empty aggregate `()` — unusual; treat as empty paren.
                    self.expect_punct(')', "to close the empty parentheses");
                    return;
                }
                self.parse_expression();
                if self.eat_punct(',') || self.peek_op2().as_deref() == Some("=>") {
                    // Aggregate.
                    self.builder.start_node_at(mark, SyntaxKind::Aggregate);
                    while self.eat_punct(',') || self.peek_op2().as_deref() == Some("=>") {
                        if self.peek_op2().as_deref() == Some("=>") {
                            self.bump();
                            self.bump();
                        }
                        if self.at_kw("others") {
                            self.start(SyntaxKind::Others);
                            self.bump();
                            self.builder.finish_node();
                        } else {
                            self.parse_expression();
                        }
                        if !self.eat_punct(',') {
                            break;
                        }
                    }
                    self.expect_punct(')', "to close the aggregate");
                    self.builder.finish_node();
                } else {
                    self.builder.start_node_at(mark, SyntaxKind::ParenExpr);
                    self.expect_punct(')', "to close the parenthesized expression");
                    self.builder.finish_node();
                }
            }
            _ => {
                self.error_here("expected an expression");
                self.bump();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::SourceDb;

    fn parse_str(src: &str) -> (ParsedFile, SourceDb) {
        let mut db = SourceDb::new();
        let id = db.add_file("t.vhd", src).unwrap();
        (parse_file(id, db.file(id)), db)
    }

    fn no_errors(p: &ParsedFile, db: &SourceDb) {
        assert!(
            !p.diags.iter().any(|d| d.severity == Severity::Error),
            "{:?}",
            p.diags.iter().map(|d| d.render(db)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn parses_entity_with_ports() {
        let src = r#"library ieee;
use ieee.std_logic_1164.all;
entity counter is
    port (
        clk : in  std_logic;
        rst : in  std_logic;
        q   : out std_logic_vector(7 downto 0)
    );
end entity;
"#;
        let (p, db) = parse_str(src);
        no_errors(&p, &db);
        let sexp = p.root.debug_sexp(&db);
        assert!(sexp.contains("(Entity"), "{}", sexp);
        assert!(sexp.contains("(PortClause"), "{}", sexp);
        assert!(sexp.contains("(PortDecl"), "{}", sexp);
        assert!(sexp.contains("(Range"), "{}", sexp);
    }

    #[test]
    fn parses_architecture_with_process() {
        let src = r#"architecture rtl of counter is
    signal q_s : unsigned(7 downto 0) := x"00";
begin
    process(clk, rst)
    begin
        if rst = '1' then
            q_s <= x"00";
        elsif rising_edge(clk) then
            q_s <= q_s + 1;
        end if;
    end process;
    q <= std_logic_vector(q_s);
end architecture;
"#;
        let (p, db) = parse_str(src);
        no_errors(&p, &db);
        let sexp = p.root.debug_sexp(&db);
        assert!(sexp.contains("(Architecture"), "{}", sexp);
        assert!(sexp.contains("(Process"), "{}", sexp);
        assert!(sexp.contains("(SensList"), "{}", sexp);
        assert!(sexp.contains("(SeqAssign"), "{}", sexp);
        assert!(sexp.contains("(ConcAssign"), "{}", sexp);
        assert!(sexp.contains("(If"), "{}", sexp);
        assert!(sexp.contains("(Call"), "{}", sexp); // rising_edge(...)
    }

    #[test]
    fn parses_case_and_for() {
        let src = r#"architecture rtl of m is
begin
    process
    begin
        case op is
            when x"00" => r <= a + b;
            when others => r <= a;
        end case;
        for i in 0 to 7 loop
            v(i) := '0';
        end loop;
        wait until rising_edge(clk);
    end process;
end;
"#;
        let (p, db) = parse_str(src);
        no_errors(&p, &db);
        let sexp = p.root.debug_sexp(&db);
        assert!(sexp.contains("(Case"), "{}", sexp);
        assert!(sexp.contains("Others"), "{}", sexp);
        assert!(sexp.contains("(For"), "{}", sexp);
        assert!(sexp.contains("(Wait"), "{}", sexp);
        assert!(sexp.contains("(VarAssign"), "{}", sexp);
    }

    #[test]
    fn parses_instantiation() {
        let src = r#"architecture rtl of top is
    component counter is
        port (clk : in std_logic; q : out std_logic_vector(7 downto 0));
    end component;
begin
    u0 : counter port map (clk => clk, q => q);
end;
"#;
        let (p, db) = parse_str(src);
        no_errors(&p, &db);
        let sexp = p.root.debug_sexp(&db);
        assert!(sexp.contains("(ComponentDecl"), "{}", sexp);
        assert!(sexp.contains("(Inst"), "{}", sexp);
        assert!(sexp.contains("(PortConn"), "{}", sexp);
    }

    #[test]
    fn expression_precedence() {
        let (p, db) = parse_str(
            "architecture rtl of m is begin p: process begin v := 1 + 2 * 3; end process; end;",
        );
        no_errors(&p, &db);
        let sexp = p.root.debug_sexp(&db);
        // `1 + 2 * 3` must nest as 1 + (2*3): the inner BinExpr contains `*`.
        let star = sexp.find("*").expect("star op present");
        let plus = sexp.find('+').expect("plus op present");
        assert!(plus < star, "{}", sexp);
    }

    #[test]
    fn lossless_roundtrip() {
        let src =
            "entity e is port (a : in bit); end;\narchitecture r of e is begin a <= not a; end;\n";
        let (p, db) = parse_str(src);
        no_errors(&p, &db);
        assert_eq!(p.root.text(&db), src);
    }
}
