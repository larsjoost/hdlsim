//! VHDL parser: tokens → lossless CST (rowan-style via [`TreeBuilder`]).
//!
//! One function per grammar production, mirroring IEEE 1076 clause names.
//! Covers VHDL-93/2002 plus the VHDL-2008 constructs in common use: library/
//! use/context clauses, entities, architectures, packages (+ instantiation),
//! configurations, all declaration forms (signal/constant/variable/shared/
//! file, type incl. enum/range/array/record/access/file/protected/units,
//! subtype with resolution function, alias, attribute decl+spec, component,
//! subprograms, for-specifications), processes (incl. `postponed`), blocks
//! (incl. `guarded`), generate for/if/case, instantiations (entity/component/
//! configuration aspects), concurrent/sequential statements (incl. selected
//! signal assignment, force/release, exit/next with labels, matching `case?`),
//! and the full expression chain (incl. qualified `T'(x)`, allocators `new`,
//! `null`, external names `<< ... >>`, conditional `??`). VHDL is
//! case-insensitive: keyword checks lowercase the token text; the CST keeps
//! the original text (lossless).
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
        "=" | "/=" | "<" | "<=" | ">" | ">=" | "?=" | "?/=" | "?<" | "?<=" | "?>" | "?>=" => {
            (2, false)
        }
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
        // Clamp: at end of input this yields the trailing Eof token, never a panic.
        let i = self.skip_trivia_from(self.pos);
        &self.tokens[i.min(self.tokens.len() - 1)]
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

    fn peek4_text(&self) -> &str {
        let i = self.skip_trivia_from(self.pos);
        let j = self.skip_trivia_from(i + 1);
        let k = self.skip_trivia_from(j + 1);
        let l = self.skip_trivia_from(k + 1);
        let t = &self.tokens[l.min(self.tokens.len() - 1)];
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

    /// Two adjacent punct tokens forming a two-char operator (`<=`, `:=`, `=>`, `/=`, `**`, `<>`),
    /// starting `ahead` non-trivia tokens from the current position.
    fn peek_op2_at(&self, ahead: usize) -> Option<String> {
        let mut i0 = self.skip_trivia_from(self.pos);
        for _ in 0..ahead {
            i0 = self.skip_trivia_from(i0 + 1);
        }
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
            "<=" | ":=" | "=>" | "/=" | "**" | "<>" | ">=" | "?=" | "?/=" | "?<" | "?>" => Some(t),
            _ => None,
        }
    }

    /// Three adjacent punct tokens forming a three-char operator (`?<=`, `?>=`).
    fn peek_op3(&self) -> Option<String> {
        let i0 = self.skip_trivia_from(self.pos);
        let i1 = self.skip_trivia_from(i0 + 1);
        let i2 = self.skip_trivia_from(i1 + 1);
        if i2 >= self.tokens.len() {
            return None;
        }
        let ts: Vec<&Token> = [i0, i1, i2].iter().map(|&i| &self.tokens[i]).collect();
        if ts.iter().any(|t| t.kind != TokenKind::Punct) {
            return None;
        }
        let t = format!(
            "{}{}{}",
            &self.src.text()[ts[0].span.start as usize..ts[0].span.end as usize],
            &self.src.text()[ts[1].span.start as usize..ts[1].span.end as usize],
            &self.src.text()[ts[2].span.start as usize..ts[2].span.end as usize]
        );
        match t.as_str() {
            "?<=" | "?>=" | "?/=" => Some(t),
            _ => None,
        }
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
            "<=" | ":=" | "=>" | "/=" | "**" | "<>" | ">=" | "?=" | "?/=" | "?<" | "?>" => Some(t),
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
        if self.pos >= self.tokens.len() {
            // Past the trailing Eof: hand it out again without advancing.
            return self.tokens[self.tokens.len() - 1].clone();
        }
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

    /// Is the current token an interface class/mode keyword?
    fn at_class_or_mode_kw(&self) -> bool {
        const KWS: &[&str] = &[
            "in", "out", "inout", "buffer", "linkage", "signal", "constant", "variable",
        ];
        self.peek().kind == TokenKind::Ident
            && KWS.contains(&self.peek_text().to_ascii_lowercase().as_str())
    }

    /// Parameter class keyword (signal/constant/variable/file) — precedes the names.
    fn at_param_class_kw(&self) -> bool {
        const KWS: &[&str] = &["signal", "constant", "variable", "file"];
        self.peek().kind == TokenKind::Ident
            && KWS.contains(&self.peek_text().to_ascii_lowercase().as_str())
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
                self.parse_context_clause();
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
    fn parse_context_clause(&mut self) {
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

    /// `configuration name of entity is { block_config } end [configuration] [name];`
    fn parse_configuration(&mut self) {
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
    fn parse_config_for(&mut self) {
        self.start(SyntaxKind::ForSpec);
        self.bump(); // for
        while !self.at_kw("end") && self.peek().kind != TokenKind::Eof {
            if self.at_kw("for") {
                self.parse_config_for(); // nested block configuration
            } else if self.at_kw("use") {
                self.bump();
                if self.at_kw("entity") || self.at_kw("configuration") {
                    self.bump();
                }
                self.parse_name(); // lib.ent / configuration name
                if self.eat_punct('(') {
                    if self.peek().kind == TokenKind::Ident {
                        self.bump(); // architecture name
                    }
                    self.expect_punct(')', "to close the architecture selection");
                }
                self.expect_punct(';', "to end the `use` clause in a configuration");
            } else {
                self.bump();
            }
        }
        self.expect_kw("end", "to close the configuration for-block");
        self.eat_kw("for");
        self.expect_punct(';', "to end the configuration for-block");
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
        self.eat_kw("architecture"); // `end [architecture] [name];`
        if self.peek().kind == TokenKind::Ident {
            self.bump(); // optional architecture name
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
        // Optional parameter class BEFORE the names: `signal s : t`,
        // `constant c : t`, `variable v : t` (subprogram parameters).
        if self.at_param_class_kw() {
            self.bump();
        }
        loop {
            // Names only; a class/mode keyword ends the name list.
            if self.peek().kind == TokenKind::Ident && !self.at_class_or_mode_kw() {
                self.parse_name();
            }
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct(':', "before the mode/type");
        // Optional mode after the colon: in/out/inout/buffer/linkage.
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
        } else if self.at_kw("range") {
            // `natural range 1 to 3` — range constraint without parens.
            self.start(SyntaxKind::Range);
            self.bump();
            self.parse_range();
            self.builder.finish_node();
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
            _ if self.at_kw("shared") => {
                // `shared variable v : t;` — VariableDecl with the `shared`
                // keyword kept in the tree.
                self.start(SyntaxKind::VariableDecl);
                self.bump(); // shared
                self.expect_kw("variable", "after `shared`");
                self.parse_simple_decl_body();
                self.builder.finish_node();
            }
            _ if self.at_kw("alias") => self.parse_alias(),
            _ if self.at_kw("attribute") => {
                // `attribute a : type;` (decl) vs `attribute a of t : l is e;` (spec)
                if self.peek3_text() == ":" {
                    self.parse_attr_decl();
                } else {
                    self.parse_attr_spec();
                }
            }
            _ if self.at_kw("file") => self.parse_file_object_decl(),
            _ if self.at_kw("use") => self.parse_use_clause(),
            _ if self.at_kw("for") => self.parse_for_spec(),
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

    /// VHDL-2008 package instantiation: `package p is new g [generic map (...)];`
    fn parse_package_inst(&mut self) {
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
        self.expect_punct(';', "to end the package instantiation");
        self.builder.finish_node();
    }

    /// `alias name [(signature)] [: subtype] is target;`
    fn parse_alias(&mut self) {
        self.start(SyntaxKind::AliasDecl);
        self.bump(); // alias
        self.parse_name();
        if self.eat_punct('(') {
            // Subprogram signature: `[...]` / `(param; ...)` — skip balanced.
            let mut depth = 1;
            while depth > 0 && self.peek().kind != TokenKind::Eof {
                if self.at_punct('(') {
                    depth += 1;
                } else if self.at_punct(')') {
                    depth -= 1;
                }
                self.bump();
            }
        }
        if self.eat_punct(':') {
            self.parse_type_mark();
            if self.at_punct('(') {
                self.parse_constraint();
            }
        }
        self.expect_kw("is", "in the alias declaration");
        self.parse_name();
        self.expect_punct(';', "to end the alias declaration");
        self.builder.finish_node();
    }

    /// `attribute name : type;`
    fn parse_attr_decl(&mut self) {
        self.start(SyntaxKind::AttrDecl);
        self.bump(); // attribute
        self.expect_ident("for the attribute name");
        self.expect_punct(':', "in the attribute declaration");
        self.parse_type_mark();
        self.expect_punct(';', "to end the attribute declaration");
        self.builder.finish_node();
    }

    /// `attribute name of target {, target} : label is expression;`
    fn parse_attr_spec(&mut self) {
        self.start(SyntaxKind::AttrSpec);
        self.bump(); // attribute
        self.expect_ident("for the attribute name");
        self.expect_kw("of", "in the attribute specification");
        loop {
            self.parse_name();
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct(':', "before the spec label");
        self.expect_ident("for the specification label");
        self.expect_kw("is", "in the attribute specification");
        self.parse_expression();
        self.expect_punct(';', "to end the attribute specification");
        self.builder.finish_node();
    }

    /// `file f [, g] : type [open mode is expr];`
    fn parse_file_object_decl(&mut self) {
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

    /// Component configuration specification:
    /// `for lbl {, lbl} | all : comp use entity|configuration ...;`
    fn parse_for_spec(&mut self) {
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
        self.expect_punct(';', "to end the for-specification");
        self.builder.finish_node();
    }

    /// `signal|constant|variable names : type [constraint] [:= init] ;`
    fn parse_simple_decl(&mut self, kind: SyntaxKind, _kw: &str) {
        self.start(kind);
        self.bump(); // keyword
        self.parse_simple_decl_body();
        self.builder.finish_node();
    }

    /// Body of a simple decl with the node already open and the keyword consumed:
    /// `names : [bus|register] type [constraint] [:= init] ;`
    fn parse_simple_decl_body(&mut self) {
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
        if self.at_punct('(') {
            self.parse_constraint();
        } else if self.at_kw("range") {
            self.start(SyntaxKind::Range);
            self.bump();
            self.parse_range();
            self.builder.finish_node();
        }
        if self.peek_op2().as_deref() == Some(":=") {
            self.bump();
            self.bump();
            self.parse_expression();
        }
        self.expect_punct(';', "to end the declaration");
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
            if self.at_kw("units") {
                self.parse_units();
            }
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
            // `record field {, field} : type [constraint] ; ... end record;`
            self.bump();
            loop {
                if self.at_kw("end") || self.peek().kind == TokenKind::Eof {
                    break;
                }
                loop {
                    if self.peek().kind == TokenKind::Ident {
                        self.parse_name();
                    }
                    if !self.eat_punct(',') {
                        break;
                    }
                }
                self.expect_punct(':', "before the record field type");
                self.parse_type_mark();
                if self.at_punct('(') {
                    self.parse_constraint();
                } else if self.at_kw("range") {
                    self.start(SyntaxKind::Range);
                    self.bump();
                    self.parse_range();
                    self.builder.finish_node();
                }
                self.expect_punct(';', "to end the record field");
            }
            self.expect_kw("end", "to close the record type");
            self.eat_kw("record");
        } else if self.at_kw("access") {
            // `access type_mark [constraint]` — pointer type.
            self.bump();
            self.parse_type_mark();
            if self.at_punct('(') {
                self.parse_constraint();
            }
        } else if self.at_kw("file") {
            // `file of type_mark` — file type.
            self.bump();
            self.expect_kw("of", "in the file type");
            self.parse_type_mark();
        } else if self.at_kw("protected") {
            // `protected ... end protected;` (+ optional `body ... end protected body;`)
            self.bump();
            while !self.at_kw("end") && self.peek().kind != TokenKind::Eof {
                self.parse_decl();
            }
            self.expect_kw("end", "to close the protected type");
            self.eat_kw("protected");
            if self.at_kw("body") {
                self.bump();
                while !self.at_kw("end") && self.peek().kind != TokenKind::Eof {
                    self.parse_decl();
                }
                self.expect_kw("end", "to close the protected body");
                self.eat_kw("protected");
                self.eat_kw("body");
            }
        } else {
            self.error_here("unsupported type definition (only enum/range/array/record-skip)");
            self.skip_to_semicolon();
        }
        self.expect_punct(';', "to end the type declaration");
        self.builder.finish_node();
    }

    /// `units base; name = value unit; ... end units;` — physical type body.
    fn parse_units(&mut self) {
        self.bump(); // units
        loop {
            if self.at_kw("end") || self.peek().kind == TokenKind::Eof {
                break;
            }
            if self.peek().kind == TokenKind::Ident {
                self.parse_name(); // base unit or derived unit name
                if self.eat_punct('=') {
                    self.parse_expression();
                    if self.peek().kind == TokenKind::Ident {
                        self.bump(); // unit name
                    }
                }
                self.expect_punct(';', "in the units declaration");
            } else {
                self.bump();
            }
        }
        self.expect_kw("end", "to close the units declaration");
        self.eat_kw("units");
    }

    fn parse_subtype_decl(&mut self) {
        self.start(SyntaxKind::SubtypeDecl);
        self.bump(); // subtype
        self.expect_ident("for the subtype name");
        self.expect_kw("is", "after the subtype name");
        // Optional resolution function: `subtype s is resolved std_logic;`
        // or `subtype s is my_resolver std_logic;`
        if self.at_kw("resolved") {
            self.bump();
        } else if self.peek().kind == TokenKind::Ident
            && self.peek2().kind == TokenKind::Ident
            && !self.peek2_text().eq_ignore_ascii_case("range")
        {
            self.bump(); // resolution function name
        }
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
    fn parse_subprogram(&mut self) {
        self.start(SyntaxKind::Function);
        self.bump(); // function | procedure
                     // Operator symbols are string literals: `function "and" (...)`.
        if self.peek().kind == TokenKind::Ident || self.peek().kind == TokenKind::StrLiteral {
            self.builder.start_node(SyntaxKind::Name);
            self.bump();
            self.builder.finish_node();
        } else {
            self.error_here("expected a subprogram name");
        }
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
        // `(r)` or multi-dimensional `(r1, r2, ...)` — each sub-range is its
        // own Range child of the outer constraint node.
        self.start(SyntaxKind::Range);
        self.expect_punct('(', "to open the constraint");
        loop {
            self.start(SyntaxKind::Range);
            self.parse_range();
            self.builder.finish_node();
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct(')', "to close the constraint");
        self.builder.finish_node();
    }

    fn parse_range(&mut self) {
        // Box constraint `range <>` (unconstrained index) — the `range`
        // keyword may come first (`range <>`) or after an index type mark
        // (`natural range <>`), so check both positions.
        if self.at_kw("range") {
            self.bump();
            if self.peek_text() == "<" && self.peek2_text() == ">" {
                self.bump();
                self.bump(); // <>
            }
            return;
        }
        self.parse_expression();
        if self.at_kw("to") || self.at_kw("downto") {
            self.bump();
            self.parse_expression();
        } else if self.at_kw("range") {
            self.bump();
            if self.peek_text() == "<" && self.peek2_text() == ">" {
                self.bump();
                self.bump(); // <>
            }
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

    // ---- concurrent statements -----------------------------------------------------

    fn parse_concurrent_stmt(&mut self) {
        // Optional label: `ident :`
        if self.at_label() {
            self.bump(); // label
            self.bump(); // ':'
        }
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
        } else if self.at_kw("entity")
            || self.at_kw("component")
            || self.at_kw("configuration")
            || (self.peek().kind == TokenKind::Ident && self.peek2().kind == TokenKind::Ident)
        {
            self.parse_instantiation();
        } else {
            // Concurrent signal assignment `target <= waveform ;` or a
            // concurrent procedure call `p(args);` — decided after the target
            // name, since `cmp_o(0) <= ...` starts like a call.
            let mark = self.builder.mark();
            self.parse_name();
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
            self.parse_expression();
            if self.at_kw("after") {
                self.bump();
                self.parse_expression();
            }
            loop {
                if self.at_kw("when") || self.at_kw("else") {
                    self.bump();
                    self.parse_expression();
                    if self.at_kw("after") {
                        self.bump();
                        self.parse_expression();
                    }
                    continue;
                }
                if self.eat_punct(',') {
                    self.parse_expression();
                    if self.at_kw("after") {
                        self.bump();
                        self.parse_expression();
                    }
                    continue;
                }
                break;
            }
            self.expect_punct(';', "to end the concurrent assignment");
            self.builder.finish_node();
        }
    }

    /// `with sel select target <= w1 when c1, w2 when c2, ...;`
    fn parse_selected_assign(&mut self) {
        self.start(SyntaxKind::SelectedAssign);
        self.bump(); // with
        self.parse_expression(); // selector
        self.expect_kw("select", "in the selected signal assignment");
        self.parse_name(); // target
        if self.peek_op2().as_deref() == Some("<=") {
            self.bump();
            self.bump();
        } else {
            self.error_here("expected '<=' in the selected signal assignment");
        }
        loop {
            self.parse_expression(); // waveform value
            if self.at_kw("after") {
                self.bump();
                self.parse_expression();
            }
            self.expect_kw("when", "before the selection choices");
            if self.at_kw("others") {
                self.start(SyntaxKind::Others);
                self.bump();
                self.builder.finish_node();
            } else {
                loop {
                    self.parse_expression();
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
    fn parse_gen_case(&mut self) {
        self.start(SyntaxKind::GenCase);
        self.bump(); // case
        self.parse_expression();
        self.eat_kw("is"); // tolerated though not standard
        self.expect_kw("generate", "after the case-generate expression");
        loop {
            if self.at_kw("end") || self.peek().kind == TokenKind::Eof {
                break;
            }
            self.expect_kw("when", "before a case-generate alternative");
            self.start(SyntaxKind::CaseItem);
            if self.at_kw("others") {
                self.start(SyntaxKind::Others);
                self.bump();
                self.builder.finish_node();
            } else {
                loop {
                    self.parse_expression();
                    if self.eat_punct('|') {
                        continue;
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
                self.error_here("expected '=>' in a case-generate alternative");
            }
            self.parse_gen_decl_part();
            while !self.at_kw("when") && !self.at_kw("end") && self.peek().kind != TokenKind::Eof {
                self.parse_concurrent_stmt();
            }
            self.builder.finish_node();
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

    /// `block [ (guard) ] [guarded] [is] decls begin concurrent-stmts end block [label] ;`
    fn parse_block(&mut self) {
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
        while !self.at_kw("begin") && !self.at_kw("end") && self.peek().kind != TokenKind::Eof {
            self.parse_decl();
        }
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

    /// True when the next token starts a declarative item (as opposed to a
    /// concurrent statement) inside a generate body. `for` is excluded: it
    /// would be ambiguous with a nested for-generate.
    fn at_decl_start(&self) -> bool {
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
    fn parse_gen_decl_part(&mut self) {
        while self.at_decl_start() && self.peek().kind != TokenKind::Eof {
            self.parse_decl();
        }
        self.eat_kw("begin");
    }

    /// `[label :] for var in range generate [decls begin] concurrent-stmts end generate [label] ;`
    fn parse_gen_for(&mut self) {
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
    fn parse_gen_if(&mut self) {
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
            // Optional loop label: `exit outer_loop when c;`
            if self.peek().kind == TokenKind::Ident && !self.at_kw("when") {
                self.bump();
            }
            if self.eat_kw("when") {
                self.parse_expression();
            }
            self.expect_punct(';', "to end the exit/next statement");
            self.builder.finish_node();
        } else if self.at_kw("force") || self.at_kw("release") {
            // VHDL-2008: `force target <= expr;` / `release target;`
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
        if self.eat_punct('?') {
            // VHDL-2008 matching case: `case? expr is ...`
            self.start(SyntaxKind::Matching);
            self.builder.finish_node();
        }
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
                let mut is_slice = false;
                if !self.at_punct(')') {
                    // Args/indices: `expr [, expr]*` where each may be a named
                    // association `name => expr`; a single `expr to|downto expr`
                    // is a slice.
                    loop {
                        if self.peek().kind == TokenKind::Ident
                            && self.peek_op2_at(1).as_deref() == Some("=>")
                        {
                            self.bump(); // association name
                            self.bump();
                            self.bump(); // =>
                        }
                        self.parse_expression();
                        if self.at_kw("to") || self.at_kw("downto") {
                            is_slice = true;
                            self.bump(); // to / downto
                            self.parse_expression();
                        }
                        if !self.eat_punct(',') {
                            break;
                        }
                    }
                }
                self.expect_punct(')', "to close the argument/index/slice list");
                let kind = if is_slice {
                    SyntaxKind::PartSelect
                } else {
                    SyntaxKind::Call
                };
                self.builder.start_node_at(mark, kind);
                self.builder.finish_node();
            } else if self.at_punct('.') {
                // `arr(i).field` — a selected name after a suffix: fold the
                // whole `name(args).field` into one Name node (the Call stays
                // nested inside it, so the lowerer can see both parts).
                self.bump();
                if self.peek().kind == TokenKind::Ident {
                    self.bump();
                } else {
                    self.error_here("expected a name after '.'");
                    break;
                }
                self.builder.start_node_at(mark, SyntaxKind::Name);
                self.builder.finish_node();
            } else if self.at_punct('\'') {
                if self.peek2().kind == TokenKind::Punct && self.peek2_text() == "(" {
                    // Qualified expression: `type_mark'(expr)`
                    self.bump(); // '
                    self.builder.start_node_at(mark, SyntaxKind::Qualified);
                    self.bump(); // (
                    self.parse_expression();
                    self.expect_punct(')', "to close the qualified expression");
                    self.builder.finish_node();
                } else {
                    self.bump(); // '
                    if self.peek().kind == TokenKind::Ident {
                        self.builder.start_node_at(mark, SyntaxKind::Attr);
                        self.bump(); // attribute name
                        self.builder.finish_node();
                    } else {
                        self.error_here("expected an attribute name after the tick");
                    }
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
        // VHDL-2008 conditional operator `??` (lowest precedence, right-assoc).
        if min_prec == 1 && self.peek_text() == "?" && self.peek2_text() == "?" {
            self.builder.start_node_at(mark, SyntaxKind::CondExpr);
            self.bump();
            self.bump();
            self.parse_expr_prec(1);
            self.builder.finish_node();
        }
    }

    fn current_binop(&self) -> Option<String> {
        if let Some(op) = self.peek_op3() {
            return binop_prec(&op).map(|_| op);
        }
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
            // Consume the operator: one to three puncts (<=, **, /=, ?<=), or a keyword.
            if self.peek().kind == TokenKind::Punct && op.len() > 1 {
                for _ in 0..op.len() {
                    self.bump();
                }
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

    /// One aggregate element: `choice [| choice...] [=> value]` where the
    /// choice is `others` or an expression.
    fn parse_agg_element(&mut self) {
        loop {
            if self.at_kw("others") {
                self.start(SyntaxKind::Others);
                self.bump();
                self.builder.finish_node();
            } else {
                self.parse_expression();
            }
            if !self.eat_punct('|') {
                break;
            }
        }
        if self.peek_op2().as_deref() == Some("=>") {
            self.bump();
            self.bump();
            if self.at_kw("others") {
                self.start(SyntaxKind::Others);
                self.bump();
                self.builder.finish_node();
            } else {
                self.parse_expression();
            }
        }
    }

    fn parse_primary(&mut self) {
        let t = self.peek().clone();
        match t.kind {
            TokenKind::IntLiteral | TokenKind::RealLiteral => {
                self.start(SyntaxKind::Literal);
                self.bump();
                // Physical literal: `100 ps` — a number followed by a time unit.
                if self.peek().kind == TokenKind::Ident
                    && matches!(
                        self.peek_text().to_ascii_lowercase().as_str(),
                        "fs" | "ps" | "ns" | "us" | "ms" | "sec" | "min" | "hr"
                    )
                {
                    self.bump();
                }
                self.builder.finish_node();
            }
            TokenKind::StrLiteral | TokenKind::CharLiteral => {
                self.start(SyntaxKind::Literal);
                self.bump();
                self.builder.finish_node();
            }
            TokenKind::Ident if self.peek_text().eq_ignore_ascii_case("null") => {
                // `null` literal (access value / empty waveform).
                self.start(SyntaxKind::Literal);
                self.bump();
                self.builder.finish_node();
            }
            TokenKind::Ident if self.peek_text().eq_ignore_ascii_case("new") => {
                // Allocator: `new type_mark` / `new type'(expr)` / `new t(range)`.
                self.start(SyntaxKind::Allocator);
                self.bump();
                self.parse_name();
                self.builder.finish_node();
            }
            TokenKind::Punct if self.peek_text() == "<" && self.peek2_text() == "<" => {
                // VHDL-2008 external name: `<< signal dut.s : std_logic >>`
                self.start(SyntaxKind::ExtName);
                self.bump();
                self.bump(); // <<
                if self.peek().kind == TokenKind::Ident {
                    self.bump(); // signal | constant | variable
                }
                self.parse_name();
                self.expect_punct(':', "in the external name");
                self.parse_type_mark();
                if self.at_punct('(') {
                    self.parse_constraint();
                }
                if self.peek_text() == ">" && self.peek2_text() == ">" {
                    self.bump();
                    self.bump(); // >>
                } else {
                    self.error_here("expected '>>' to close the external name");
                }
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
                // Aggregate if the first element is followed by `,`, `|`
                // (more choices) or `=>` (named association).
                let mut is_agg =
                    self.peek_op2().as_deref() == Some("=>") || self.peek_text() == "|";
                let mut first_comma = false;
                if self.eat_punct(',') {
                    is_agg = true;
                    first_comma = true;
                }
                if is_agg {
                    // Aggregate: elements are `choice [| choice...] [=> value]`.
                    self.builder.start_node_at(mark, SyntaxKind::Aggregate);
                    // Finish the first element (extra choices / `=> value`).
                    while self.eat_punct('|') {
                        if self.at_kw("others") {
                            self.start(SyntaxKind::Others);
                            self.bump();
                            self.builder.finish_node();
                        } else {
                            self.parse_expression();
                        }
                    }
                    if self.peek_op2().as_deref() == Some("=>") {
                        self.bump();
                        self.bump();
                        if self.at_kw("others") {
                            self.start(SyntaxKind::Others);
                            self.bump();
                            self.builder.finish_node();
                        } else {
                            self.parse_expression();
                        }
                    }
                    if first_comma {
                        // The separating `,` is already consumed: parse the
                        // remaining elements.
                        loop {
                            self.parse_agg_element();
                            if !self.eat_punct(',') {
                                break;
                            }
                        }
                    } else {
                        while self.eat_punct(',') {
                            self.parse_agg_element();
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
    fn parses_named_aggregates() {
        // Named associations `(f => v, ...)`, positional, `others`, and
        // choice lists `1 | 2 => v` must all parse losslessly.
        let sexp = ok(r#"architecture rtl of m is
    type rt is record a : bit; b : bit; end record;
    signal r : rt;
begin
    r <= (a => '1', b => '0');
    r <= ('1', '0');
    r <= (others => '0');
    r <= (1 | 2 => '1', others => '0');
end architecture;
"#);
        assert!(sexp.contains("(Aggregate"), "{}", sexp);
        assert!(sexp.contains("(Others"), "{}", sexp);
    }

    #[test]
    fn parses_all_end_architecture_forms() {
        // `end [architecture] [name];` — all four forms are valid VHDL.
        for tail in [
            "end;",
            "end rtl;",
            "end architecture;",
            "end architecture rtl;",
            "END ARCHITECTURE behavior;",
        ] {
            let src = format!("architecture rtl of m is\nbegin\n{}\n", tail);
            let sexp = ok(&src);
            assert!(sexp.contains("(Architecture"), "{}: {}", tail, sexp);
        }
    }

    #[test]
    fn parses_generate_and_block_forms() {
        // All `end ... generate;` tails, generate declarative parts (with and
        // without `begin`), elsif/else branches in if-generate, and
        // case-generate (the non-standard `case x is generate` is tolerated).
        let src = r#"architecture rtl of m is
    signal a, b : bit_vector(3 downto 0);
begin
    g1: for i in 0 to 3 generate
        b(i) <= not a(i);
    end generate g1;
    g2: for i in 3 downto 0 generate
        signal t : bit;
    begin
        t <= a(i);
        b(i) <= t;
    end for generate;
    g3: if true generate
        b <= a;
    elsif false generate
        b <= not a;
    else generate
        b <= (others => '0');
    end if generate;
    g4: case a is generate
        when x"0" => b <= a;
        when others => b <= (others => '1');
    end case generate;
    blk: block
        signal t : bit;
    begin
        t <= '1';
    end block blk;
end architecture;
"#;
        let sexp = ok(src);
        assert_eq!(sexp.matches("(GenFor").count(), 2, "{}", sexp);
        assert_eq!(sexp.matches("(GenIf").count(), 1, "{}", sexp);
        assert_eq!(sexp.matches("(GenCase").count(), 1, "{}", sexp);
        assert_eq!(sexp.matches("(Block").count(), 1, "{}", sexp);
        assert_eq!(sexp.matches("(SignalDecl").count(), 3, "{}", sexp);
        assert_eq!(sexp.matches("(CaseItem").count(), 2, "{}", sexp);
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

    /// Parse, assert zero errors + losslessness, return the s-expression.
    fn ok(src: &str) -> String {
        let (p, db) = parse_str(src);
        no_errors(&p, &db);
        assert_eq!(p.root.text(&db), src, "tree must be lossless");
        p.root.debug_sexp(&db)
    }

    #[test]
    fn parses_configuration_and_context_clause() {
        let sexp = ok(r#"context ieee.std_logic_1164.all;
configuration cfg of top is
  for rtl
    for u0 : counter
      use entity work.counter(rtl);
    end for;
  end for;
end configuration;
"#);
        assert!(sexp.contains("(ContextClause"), "{}", sexp);
        assert!(sexp.contains("(ConfigDecl"), "{}", sexp);
        assert_eq!(sexp.matches("(ForSpec").count(), 2, "{}", sexp);
    }

    #[test]
    fn parses_package_instantiation() {
        let sexp = ok(r#"library ieee;
package p is new ieee.std_logic_1164.all generic map (W => 8);
"#);
        let _ = sexp;
        let sexp = ok(
            r#"package my_pkg is new work.generic_pkg generic map (W => 8);
"#,
        );
        assert!(sexp.contains("(PackageInst"), "{}", sexp);
    }

    #[test]
    fn parses_generic_package_declaration() {
        let sexp = ok(r#"package gen_pkg is
  generic (WIDTH : natural := 8; DEPTH : positive := 4);
  constant C_W : natural := WIDTH;
end package;
package body gen_pkg is
begin
end package body;
architecture rtl of top is
  package inst_pkg is new work.gen_pkg generic map (WIDTH => 16);
  use work.inst_pkg.all;
begin
end architecture;
"#);
        // one GenericClause in the package declaration
        assert_eq!(sexp.matches("(GenericClause").count(), 1, "{}", sexp);
        assert!(sexp.contains("(PackageInst"), "{}", sexp);
        assert_eq!(sexp.matches("(UseClause").count(), 1, "{}", sexp);
    }

    #[test]
    fn parses_rich_type_declarations() {
        let sexp = ok(r#"architecture rtl of m is
    type rec_t is record
        a : std_logic;
        b : integer range 0 to 3;
    end record;
    type ptr_t is access std_logic_vector(7 downto 0);
    type file_t is file of std_logic_vector;
    file f : file_t open read_mode is "in.txt";
    type prot_t is protected
        procedure p(x : in integer);
    end protected;
    type time_t is range 0 to 1000
        units
            fs;
            ps = 1000 fs;
            ns = 1000 ps;
        end units;
    subtype resolved_s is resolved std_logic;
    subtype custom_res is my_resolver std_logic;
    signal s : register std_logic;
    signal b : bus unsigned(3 downto 0);
    shared variable sv : integer;
    alias al : std_logic is s;
    attribute keep : boolean;
    attribute keep of s : signal is true;
    use work.pkg.all;
    for u0 : counter use entity work.counter(rtl);
begin
end;
"#);
        for node in [
            "(TypeDecl",
            "(FileDecl",
            "(AliasDecl",
            "(AttrDecl",
            "(AttrSpec",
            "(VariableDecl",
            "(ForSpec",
        ] {
            assert!(sexp.contains(node), "{} missing in {}", node, sexp);
        }
    }

    #[test]
    fn parses_selected_signal_assignment() {
        let sexp = ok(r#"architecture rtl of m is
begin
    with sel select
        y <= a when 0,
             b when 1 | 2,
             c when others;
end;
"#);
        assert!(sexp.contains("(SelectedAssign"), "{}", sexp);
        assert!(sexp.contains("Others"), "{}", sexp);
    }

    #[test]
    fn parses_generate_case() {
        let sexp = ok(r#"architecture rtl of m is
begin
    case g_sel generate
        when 0 =>
            y <= a;
        when 1 | 2 =>
            y <= b;
        when others =>
            y <= c;
    end generate;
end;
"#);
        assert!(sexp.contains("(GenCase"), "{}", sexp);
        assert_eq!(sexp.matches("(CaseItem").count(), 3, "{}", sexp);
    }

    #[test]
    fn parses_concurrent_procedure_call_postponed_and_guarded() {
        let sexp = ok(r#"architecture rtl of m is
begin
    init(x);
    postponed process(clk) begin
        q <= d;
    end process;
    blk : block (g = '1') guarded is
    begin
        q <= d;
    end block;
end;
"#);
        assert!(sexp.contains("(CallStmt"), "{}", sexp);
        assert!(sexp.contains("postponed"), "{}", sexp);
        assert!(sexp.contains("guarded"), "{}", sexp);
    }

    #[test]
    fn parses_exit_next_labels_force_release_matching_case() {
        let sexp = ok(r#"architecture rtl of m is
begin
    p : process
    begin
        outer : loop
            exit outer when done = '1';
            next when skip;
        end loop;
        force q <= d;
        release q;
        case? op is
            when 0 to 3 | 7 => r <= a;
            when others => r <= b;
        end case;
    end process;
end;
"#);
        assert_eq!(sexp.matches("(ExitStmt").count(), 2, "{}", sexp);
        // force, release, and the two case-alternative assigns.
        assert_eq!(sexp.matches("(SeqAssign").count(), 4, "{}", sexp);
        assert!(sexp.contains("(Case "), "{}", sexp);
    }

    #[test]
    fn parses_qualified_allocator_null_extname_condop() {
        let sexp = ok(r#"architecture rtl of m is
    type ptr is access integer;
    variable p : ptr;
begin
    p : process
        variable v : integer;
    begin
        v := integer'(42);
        p := new integer;
        p := new integer'(7);
        if v = 0 then v := 1; end if;
        v := (a ?? b);
        if p = null then v := 0; end if;
    end process;
    q <= << signal dut.internal_sig : std_logic >>;
end;
"#);
        assert!(sexp.contains("(Qualified"), "{}", sexp);
        assert_eq!(sexp.matches("(Allocator").count(), 2, "{}", sexp);
        assert!(sexp.contains("(CondExpr"), "{}", sexp);
        assert!(sexp.contains("(ExtName"), "{}", sexp);
    }

    #[test]
    fn parses_instantiation_with_configuration_aspect() {
        let sexp = ok(r#"architecture rtl of top is
begin
    u0 : configuration work.cfg port map (clk => clk);
end;
"#);
        assert!(sexp.contains("(Inst"), "{}", sexp);
    }

    #[test]
    fn never_panics_on_garbage() {
        for src in [
            "",
            "entity",
            "configuration",
            "architecture r of e is begin with select",
            "package p is new",
            "type t is record",
            "<< signal",
            "case? ",
            "v := new",
            "alias",
            "attribute",
            "for all :",
        ] {
            let _ = parse_str(src);
        }
    }
}
