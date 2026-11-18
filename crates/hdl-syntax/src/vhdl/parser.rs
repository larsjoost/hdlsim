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
        in_waveform: 0,
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
    /// >0 while parsing a waveform value, where `when`/`else` belongs to the
    /// waveform (conditional signal assignment), not to a conditional
    /// expression.
    in_waveform: u32,
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

    /// Parameter class keyword (signal/constant/variable/file/type) — precedes the names.
    fn at_param_class_kw(&self) -> bool {
        const KWS: &[&str] = &["signal", "constant", "variable", "file", "type"];
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

    /// Context declaration: `context name is <context clause items> end [context] [name];`
    fn parse_context_decl(&mut self) {
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
    /// Block spec: `arch_name`, `label : comp`, `all : comp`, `others : comp`.
    fn parse_config_for(&mut self) {
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
    fn parse_config_binding(&mut self) {
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
        // `constant c : t`, `variable v : t` (subprogram parameters),
        // `type T` (VHDL-2008 type generic in a generic clause).
        let class = if self.at_param_class_kw() {
            let t = self.peek_text().to_ascii_lowercase();
            self.bump();
            Some(t)
        } else {
            None
        };
        loop {
            // Names only; a class/mode keyword ends the name list.
            if self.peek().kind == TokenKind::Ident && !self.at_class_or_mode_kw() {
                self.parse_name();
            }
            if !self.eat_punct(',') {
                break;
            }
        }
        // `type T;` — a type generic has no mode or type mark.
        // VHDL-2019: `type T is <subtype indication>` (e.g.
        // `type V is array (type is range <>) of type is private`).
        if class.as_deref() == Some("type") && !self.at_punct(':') {
            if self.at_kw("is") {
                self.bump();
                // Skip the subtype indication, tracking bracket/paren depth so
                // the closing `)` of the generic list stays for the caller.
                let mut depth = 0;
                while self.peek().kind != TokenKind::Eof {
                    if self.at_punct('(') || self.at_punct('[') {
                        depth += 1;
                    } else if self.at_punct(')') || self.at_punct(']') {
                        if depth == 0 {
                            break;
                        }
                        depth -= 1;
                    } else if depth == 0 && self.at_punct(';') {
                        break;
                    }
                    self.bump();
                }
            } else {
                // Bare `type T` box — stop at `;` or at the `)` closing the
                // generic list (whichever comes first at depth zero).
                let mut depth = 0;
                while self.peek().kind != TokenKind::Eof {
                    if self.at_punct('(') || self.at_punct('[') {
                        depth += 1;
                    } else if self.at_punct(')') || self.at_punct(']') {
                        if depth == 0 {
                            break;
                        }
                        depth -= 1;
                    } else if depth == 0 && self.at_punct(';') {
                        break;
                    }
                    self.bump();
                }
            }
            self.builder.finish_node();
            return;
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
        while self.at_punct('(') {
            self.parse_constraint();
        }
        // Port kind: `signal q : out resolved_byte bus)`.
        if self.at_kw("bus") || self.at_kw("register") {
            self.bump();
        }
        if self.at_kw("range") {
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
            // VHDL-2008 `pure`/`impure` function prefixes (kept as tokens in
            // the subprogram node; semantics unchanged for simulation).
            _ if self.at_kw("pure") || self.at_kw("impure") => {
                self.parse_subprogram();
            }
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
            _ if self.at_kw("group") => {
                // `group name [is|: template] (...);` — parsed loosely, skipped.
                self.bump();
                if self.peek().kind == TokenKind::Ident {
                    self.bump();
                }
                self.eat_kw("is");
                self.eat_punct(':');
                if self.peek().kind == TokenKind::Ident {
                    self.bump();
                }
                if self.eat_punct('(') {
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
                self.expect_punct(';', "to end the group declaration");
            }
            _ if self.at_kw("disconnect") => {
                // `disconnect sig_list : type_mark [after expr];`
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
        self.eat_punct(';'); // may be absent when nested in a generic clause
        self.builder.finish_node();
    }

    /// `alias name [(signature)] [: subtype] is target;`
    fn parse_alias(&mut self) {
        self.start(SyntaxKind::AliasDecl);
        self.bump(); // alias
        self.parse_name();
        if self.eat_punct('(') {
            // Subprogram signature: `(param; ...)` — skip balanced.
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
        if self.eat_punct('[') {
            // Subprogram signature in brackets: `alias a is f[T1, T2];`
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
        if self.eat_punct(':') {
            self.parse_type_mark();
            if self.at_punct('(') {
                self.parse_constraint();
            }
        }
        self.expect_kw("is", "in the alias declaration");
        self.parse_name();
        if self.eat_punct('[') {
            // Subprogram signature after the name: `alias a is f[T, boolean return boolean];`
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
        // Optional signature on the target: `alias a is f[T1, T2];`
        if self.at_punct('[') {
            let mut depth = 1;
            self.bump();
            while depth > 0 && self.peek().kind != TokenKind::Eof {
                if self.at_punct('[') {
                    depth += 1;
                } else if self.at_punct(']') {
                    depth -= 1;
                }
                self.bump();
            }
        }
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

    fn parse_type_decl(&mut self) {
        self.start(SyntaxKind::TypeDecl);
        self.bump(); // type
        self.expect_ident("for the type name");
        // Incomplete (forward) type declaration: `type ListType;`
        if self.eat_punct(';') {
            self.builder.finish_node();
            return;
        }
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
            } else if self.at_kw("range") {
                // Range-constrained element type: `... of integer range 3 downto 0`.
                self.start(SyntaxKind::Range);
                self.bump();
                self.parse_range();
                self.builder.finish_node();
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
            if self.peek().kind == TokenKind::Ident {
                self.bump(); // optional record name
            }
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
            // `protected ... end protected [name];` and the separate-body form
            // `type X is protected body ... end protected body [X];`
            self.bump();
            let is_body = self.at_kw("body");
            if is_body {
                self.bump();
            }
            while !self.at_kw("end") && self.peek().kind != TokenKind::Eof {
                self.parse_decl();
            }
            self.expect_kw("end", "to close the protected type");
            self.eat_kw("protected");
            // `end protected [body] [name];`
            self.eat_kw("body");
            if self.peek().kind == TokenKind::Ident {
                self.bump(); // optional type name
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
        if self.peek().kind == TokenKind::Ident {
            self.bump(); // optional unit name: `end units length;`
        }
    }

    fn parse_subtype_decl(&mut self) {
        self.start(SyntaxKind::SubtypeDecl);
        self.bump(); // subtype
        self.expect_ident("for the subtype name");
        self.expect_kw("is", "after the subtype name");
        // Optional resolution function: `subtype s is resolved std_logic;`,
        // `subtype s is my_resolver std_logic;`, or the parenthesized form
        // `subtype s is (my_resolver) std_logic;`.
        if self.at_kw("resolved") {
            self.bump();
        } else if self.at_punct('(') {
            self.bump();
            if self.peek().kind == TokenKind::Ident {
                self.bump();
            }
            self.expect_punct(')', "to close the resolution function");
        } else if self.peek().kind == TokenKind::Ident
            && self.peek2().kind == TokenKind::Ident
            && !self.peek2_text().eq_ignore_ascii_case("range")
        {
            self.bump(); // resolution function name
        }
        self.parse_type_mark();
        // Type attribute in the indication: `subtype E is V'element;` /
        // `subtype I is V'index;` (VHDL-2019).
        if self.at_punct('\'') {
            self.bump();
            if self.peek().kind == TokenKind::Ident {
                self.bump(); // attribute name
            } else {
                self.error_here("expected an attribute name after the tick");
            }
        }
        while self.at_punct('(') {
            self.parse_constraint();
        }
        if self.at_kw("range") {
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
        // Optional VHDL-2008 purity prefix: `impure function f ...`.
        if self.at_kw("pure") || self.at_kw("impure") {
            self.bump();
        }
        self.bump(); // function | procedure
                     // Operator symbols are string literals: `function "and" (...)`.
        if self.peek().kind == TokenKind::Ident || self.peek().kind == TokenKind::StrLiteral {
            self.builder.start_node(SyntaxKind::Name);
            self.bump();
            self.builder.finish_node();
        } else {
            self.error_here("expected a subprogram name");
        }
        // VHDL-2008: `procedure p parameter (x : t);` — the interface list may
        // be introduced by the `parameter` keyword.
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
            // Optional name — an operator symbol is a string literal: `end function "ABS";`
            if self.peek().kind == TokenKind::Ident || self.peek().kind == TokenKind::StrLiteral {
                self.bump(); // name
            }
        }
        // Tolerate a missing ';' right before the ')' of a generic list
        // (some sources rely on it).
        if !self.eat_punct(';') && !self.at_punct(')') {
            self.error_here("expected ';' to end the subprogram");
        }
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
            // Subtype attribute as a type mark: `A'subtype` — keep the base
            // name as the type mark, consume the tick + attribute name.
            if self.at_punct('\'') && self.peek2().kind == TokenKind::Ident {
                self.bump();
                self.bump();
            }
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
        }
        // Box `<>` may also follow a `range` keyword already consumed by the
        // caller (type/subtype declarations bump `range` themselves).
        if self.peek_text() == "<" && self.peek2_text() == ">" {
            self.bump();
            self.bump(); // <>
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
            } else {
                // `subtype_name range lo to hi` / `... range lo downto hi`.
                self.parse_expression();
                if self.at_kw("to") || self.at_kw("downto") {
                    self.bump();
                    self.parse_expression();
                }
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
            // VHDL-2008 bare block as a generate-alternative body:
            // `begin concurrent-stmts end [label] ;` (checked before the
            // two-idents instantiation heuristic: `begin q <= ...`).
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
            let mark = self.builder.mark();
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
            // Concurrent configuration instantiation:
            // `lbl : configuration lib.cfg [generic map (...)] [port map (...)];`
            if self.at_kw("configuration") {
                self.bump();
                self.parse_name();
            }
            // Component instantiation with a selected name:
            // `lbl: lib.pkg.comp [generic map (...)] port map (...);`
            if self.at_kw("generic") || self.at_kw("port") {
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
        // Target may be an aggregate `(s1, s2, ...)`.
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
            self.parse_name(); // target
        }
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
            let mut labeled = false;
            if self.at_kw("others") {
                self.start(SyntaxKind::Others);
                self.bump();
                self.builder.finish_node();
            } else {
                // VHDL-2008 alternative label: `when lbl : choice =>`.
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
            }
            if self.peek_op2().as_deref() == Some("=>") {
                self.bump();
                self.bump();
            } else {
                self.error_here("expected '=>' in a case-generate alternative");
            }
            if labeled && self.at_kw("begin") {
                // VHDL-2008 labeled alternative with a bare block body:
                // `when lbl : choice => begin concurrent-stmts end lbl;`
                self.start(SyntaxKind::Block);
                self.bump(); // begin
                while !self.at_kw("end") && self.peek().kind != TokenKind::Eof {
                    self.parse_concurrent_stmt();
                }
                self.expect_kw("end", "to close the block");
                if self.peek().kind == TokenKind::Ident {
                    self.bump(); // alternative label
                }
                self.expect_punct(';', "to end the block");
                self.builder.finish_node();
            } else {
                self.parse_gen_decl_part();
                while !self.at_kw("when")
                    && !self.at_kw("end")
                    && self.peek().kind != TokenKind::Eof
                {
                    self.parse_concurrent_stmt();
                }
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
            // Block header (VHDL-2002+): `generic (...);` / `port (...);`
            // and the header maps `generic map (...);` / `port map (...);`.
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
        self.eat_kw("postponed"); // `end postponed process;`
        self.eat_kw("process");
        if self.peek().kind == TokenKind::Ident {
            self.bump(); // optional label
        }
        self.expect_punct(';', "to end the process");
        self.builder.finish_node();
    }

    /// `label : comp [generic map (...)] port map (...);` or `entity lib.name(arch) ...`.
    fn parse_instantiation(&mut self, label: Option<crate::cst::Mark>) {
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

    /// `( assoc {, assoc} )` where assoc is `.name => expr`, `name => expr`,
    /// or a positional expression.
    /// Is the association at the current position named (`formal => actual`)?
    /// The formal may be a plain name, an indexed/sliced name (`BitsA(0) =>`),
    /// or a selected name (`x.y(2) =>`).
    fn assoc_is_named(&self) -> bool {
        let mut i = self.skip_trivia_from(self.pos);
        // The formal may also be an operator symbol: `"<"(a => b, ...)`.
        if self.tokens.get(i).map_or(false, |t| {
            matches!(t.kind, TokenKind::Ident | TokenKind::StrLiteral)
        }) {
            i = self.skip_trivia_from(i + 1);
            // Skip `(...)` / `[...]` suffixes and `.name` selects.
            loop {
                let Some(t) = self.tokens.get(i) else {
                    return false;
                };
                let txt = &self.src.text()[t.span.start as usize..t.span.end as usize];
                if txt == "(" || txt == "[" {
                    let close = if txt == "(" { ")" } else { "]" };
                    let mut depth = 0usize;
                    loop {
                        let Some(t2) = self.tokens.get(i) else {
                            return false;
                        };
                        let x = &self.src.text()[t2.span.start as usize..t2.span.end as usize];
                        if x == txt {
                            depth += 1;
                        } else if x == close {
                            depth -= 1;
                            if depth == 0 {
                                i = self.skip_trivia_from(i + 1);
                                break;
                            }
                        }
                        i += 1;
                    }
                } else if txt == "." {
                    i = self.skip_trivia_from(i + 1);
                    if self
                        .tokens
                        .get(i)
                        .map_or(false, |t| t.kind == TokenKind::Ident)
                    {
                        i = self.skip_trivia_from(i + 1);
                    } else {
                        return false;
                    }
                } else {
                    break;
                }
            }
            let a = self.tokens.get(i);
            let b = self.tokens.get(self.skip_trivia_from(i + 1));
            return matches!((a, b), (Some(x), Some(y)) if x.kind == TokenKind::Punct && y.kind == TokenKind::Punct
                && &self.src.text()[x.span.start as usize..x.span.end as usize] == "="
                && &self.src.text()[y.span.start as usize..y.span.end as usize] == ">");
        }
        false
    }

    /// Bump the tokens of a named-association formal (`name`, possibly with
    /// `(...)` / `.name` suffixes) without wrapping them in a node.
    fn skip_assoc_formal(&mut self) {
        if matches!(self.peek().kind, TokenKind::Ident | TokenKind::StrLiteral) {
            self.bump();
        }
        loop {
            let txt = self.peek_text().to_string();
            if txt == "(" {
                let mut depth = 0usize;
                while self.peek().kind != TokenKind::Eof {
                    let t = self.peek_text().to_string();
                    self.bump();
                    if t == "(" {
                        depth += 1;
                    } else if t == ")" {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                }
            } else if txt == "." {
                self.bump();
                if self.peek().kind == TokenKind::Ident {
                    self.bump();
                }
            } else {
                break;
            }
        }
    }

    fn parse_assoc_list(&mut self) {
        self.expect_punct('(', "to open the association list");
        while !self.at_punct(')') && self.peek().kind != TokenKind::Eof {
            self.start(SyntaxKind::PortConn);
            let named = self.at_punct('.') || self.assoc_is_named();
            if named {
                if self.eat_punct('.') {
                    let m = self.builder.mark();
                    if self.peek().kind == TokenKind::Ident {
                        self.bump();
                    }
                    self.builder.start_node_at(m, SyntaxKind::Name);
                    self.builder.finish_node();
                } else {
                    // Formal may be an indexed/sliced name: `BitsA(0) => x`.
                    self.parse_name();
                }
                if self.peek_op2().as_deref() == Some("=>") {
                    self.bump();
                    self.bump();
                } else {
                    self.error_here("expected '=>' in the association");
                }
            }
            if !self.at_punct(')') {
                if self.peek_text() == "<" && self.peek2_text() == ">" {
                    self.bump();
                    self.bump(); // `<>` box (open association)
                } else {
                    self.parse_expression();
                }
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
        } else if self.at_punct('(') {
            // VHDL-2008 parenthesized target list: `(a, b) := f(...);`
            let mark = self.builder.mark();
            self.bump(); // (
            loop {
                self.parse_postfix_name();
                if !self.eat_punct(',') {
                    break;
                }
            }
            self.expect_punct(')', "to close the target list");
            if self.peek_op2().as_deref() == Some("<=") || self.peek_op2().as_deref() == Some(":=")
            {
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
        } else {
            // Assignment or procedure call: parse the target/name, then decide.
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

    // ---- names and expressions -----------------------------------------------------

    /// A name: `ident` with selected (`.b`), indexed (`(i)`), sliced
    /// (`(msb downto lsb)`), called (`(args)`), or attributed (`'attr`) suffixes.
    fn parse_name(&mut self) {
        let mark = self.builder.mark();
        if matches!(
            self.peek().kind,
            TokenKind::Ident | TokenKind::StrLiteral | TokenKind::CharLiteral
        ) {
            self.start(SyntaxKind::Name);
            self.bump();
            while self.at_punct('.') {
                self.bump();
                // Selected name may be a quoted operator: `pkg."+"(a, b)`.
                if matches!(
                    self.peek().kind,
                    TokenKind::Ident | TokenKind::StrLiteral | TokenKind::CharLiteral
                ) {
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
                        if self.assoc_is_named() {
                            // Named association; the formal may be indexed:
                            // `a(0) => b`. Bump the formal's tokens without
                            // wrapping them in a Name node so the lowerer keeps
                            // treating it as a skipped formal, not an argument.
                            self.skip_assoc_formal();
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
                if matches!(
                    self.peek().kind,
                    TokenKind::Ident | TokenKind::StrLiteral | TokenKind::CharLiteral
                ) {
                    self.bump();
                } else {
                    self.error_here("expected a name after '.'");
                    break;
                }
                self.builder.start_node_at(mark, SyntaxKind::Name);
                self.builder.finish_node();
            } else if self.at_punct('[') {
                // Subprogram signature in brackets: `f[return T]'encoding`
                // — skip the balanced `[...]`, then keep looping so a
                // following `'attr` suffix is parsed too.
                let mut depth = 0;
                while self.peek().kind != TokenKind::Eof {
                    if self.at_punct('[') {
                        depth += 1;
                    } else if self.at_punct(']') {
                        depth -= 1;
                        if depth == 0 {
                            self.bump();
                            break;
                        }
                    }
                    self.bump();
                }
            } else if self.at_punct('\'') {
                if self.peek2().kind == TokenKind::Punct && self.peek2_text() == "(" {
                    // Qualified expression: `type_mark'(expr)`
                    self.bump(); // '
                    self.builder.start_node_at(mark, SyntaxKind::Qualified);
                    self.bump(); // (
                                 // The operand may be an aggregate: `integer_vector'(1, 7)`,
                                 // `rec_t'(ID => x)`, `s'(1 to N => ' ')`.
                    self.parse_expression();
                    loop {
                        if self.peek_op2().as_deref() == Some("=>") {
                            self.bump();
                            self.bump();
                            self.parse_expression();
                        } else if self.at_kw("to") || self.at_kw("downto") {
                            self.bump();
                            self.parse_expression();
                        } else if self.eat_punct(',') {
                            self.parse_agg_element();
                        } else {
                            break;
                        }
                    }
                    self.expect_punct(')', "to close the qualified expression");
                    self.builder.finish_node();
                } else {
                    self.bump(); // '
                    if matches!(self.peek().kind, TokenKind::Ident | TokenKind::CharLiteral) {
                        self.builder.start_node_at(mark, SyntaxKind::Attr);
                        self.bump(); // attribute name / char literal
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

    /// Parse an expression in waveform position, where a following
    /// `when`/`else` belongs to the waveform, not to a conditional expression.
    fn parse_waveform_value(&mut self) {
        self.in_waveform += 1;
        self.parse_expression();
        self.in_waveform -= 1;
    }

    fn parse_expr_prec(&mut self, min_prec: u8) {
        let mark = self.builder.mark();
        self.parse_unary();
        self.parse_binops(mark, min_prec);
        if min_prec != 1 {
            return;
        }
        // VHDL-2008 conditional operator `??` (lowest precedence, right-assoc).
        if self.peek_text() == "?" && self.peek2_text() == "?" {
            self.builder.start_node_at(mark, SyntaxKind::CondExpr);
            self.bump();
            self.bump();
            self.parse_expr_prec(1);
            self.builder.finish_node();
            return;
        }
        // VHDL-2008 conditional expression: `v when c else e [when c2 else e2]`.
        // Not in waveform position (there `when` starts a conditional waveform).
        if self.in_waveform == 0 && self.at_kw("when") {
            self.builder.start_node_at(mark, SyntaxKind::CondExpr);
            self.bump(); // when
            self.parse_expr_prec(1); // condition
            self.expect_kw("else", "in the conditional expression");
            self.parse_expr_prec(1); // else branch (chains further when/else)
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

    /// Waveform elements of a signal assignment: `expr [after t] {, expr
    /// [after t]}`, optionally followed by the conditional form
    /// `when cond else expr ...`.
    /// Optional delay mechanism prefix: `[reject t] (transport|inertial)`, and
    /// the VHDL-93 `guarded` assignment keyword.
    fn eat_delay_mechanism(&mut self) {
        // `[reject t] (transport|inertial)` and `guarded` may appear in any
        // order (`guarded reject 2 ns inertial`).
        loop {
            if self.at_kw("reject") {
                self.bump();
                self.parse_expression();
                continue;
            }
            if self.at_kw("transport") || self.at_kw("inertial") || self.at_kw("guarded") {
                self.bump();
                continue;
            }
            break;
        }
    }

    fn parse_waveform_elements(&mut self) {
        self.eat_delay_mechanism();
        self.parse_waveform_value();
        loop {
            if self.at_kw("after") {
                self.bump();
                self.parse_waveform_value();
            }
            // Conditional waveform: `x <= a when c else b;` (flat tokens,
            // lowered like the concurrent form).
            if self.at_kw("when") || self.at_kw("else") {
                self.bump();
                self.parse_waveform_value();
                continue;
            }
            if self.eat_punct(',') {
                self.eat_delay_mechanism();
                self.parse_waveform_value();
                continue;
            }
            break;
        }
    }

    fn parse_unary(&mut self) {
        if self.at_punct('-') || self.at_punct('+') {
            self.start(SyntaxKind::UnaryExpr);
            self.bump();
            self.parse_unary();
            self.builder.finish_node();
        } else if self.peek_text() == "?" && self.peek2_text() == "?" {
            // VHDL-2008 condition operator `??x` (boolean conversion).
            self.start(SyntaxKind::UnaryExpr);
            self.bump();
            self.bump();
            self.parse_unary();
            self.builder.finish_node();
        } else if self.at_kw("not")
            || self.at_kw("abs")
            // Prefix reduction operators: `and x`, `or x`, `xor x`, ...
            || matches!(
                self.peek_text().to_ascii_lowercase().as_str(),
                "and" | "or" | "xor" | "nand" | "nor" | "xnor"
            )
        {
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
    /// One aggregate choice: `others`, an expression, or a range choice
    /// (`a to b`, `a downto b`, `range <>`).
    fn parse_agg_choice(&mut self) {
        if self.at_kw("others") {
            self.start(SyntaxKind::Others);
            self.bump();
            self.builder.finish_node();
            return;
        }
        if self.at_kw("range") {
            self.bump();
            if self.peek_op2().as_deref() == Some("<>") {
                self.bump();
                self.bump();
            }
            return;
        }
        self.parse_expression();
        if self.at_kw("to") || self.at_kw("downto") {
            self.bump();
            self.parse_expression();
        }
    }

    fn parse_agg_element(&mut self) {
        loop {
            self.parse_agg_choice();
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
                // Sized bit-string literal: `32X"0"` (VHDL-2008) — number
                // followed by a based string.
                if self.peek().kind == TokenKind::StrLiteral
                    && self.peek_text().len() >= 2
                    && matches!(
                        self.peek_text().as_bytes()[0],
                        b'x' | b'X' | b'b' | b'B' | b'o' | b'O' | b'd' | b'D'
                    )
                    && self.peek_text().as_bytes()[1] == b'"'
                {
                    self.bump();
                }
                // Sized bit-string literal: `32X"0"`, `32SB"1"` — the size
                // integer is directly followed by an optional `s` (signed),
                // the base letter, and the string.
                if self.peek().kind == TokenKind::Ident
                    && (self.peek_text().len() == 1
                        || (self.peek_text().len() == 2
                            && matches!(self.peek_text().as_bytes()[0], b's' | b'S')))
                    && matches!(
                        self.peek_text().as_bytes()[self.peek_text().len() - 1],
                        b'x' | b'X' | b'b' | b'B' | b'o' | b'O' | b'd' | b'D'
                    )
                    && self.peek2().kind == TokenKind::StrLiteral
                {
                    self.bump();
                    self.bump();
                }
                // Physical literal: `100 ps`, `1.5 cm` — a number followed by a
                // physical unit (time, length, frequency).
                if self.peek().kind == TokenKind::Ident
                    && matches!(
                        self.peek_text().to_ascii_lowercase().as_str(),
                        "fs" | "ps"
                            | "ns"
                            | "us"
                            | "ms"
                            | "sec"
                            | "s"
                            | "min"
                            | "hr"
                            | "um"
                            | "mm"
                            | "cm"
                            | "m"
                            | "km"
                            | "nm"
                            | "pm"
                            | "hz"
                            | "khz"
                            | "mhz"
                            | "ghz"
                    )
                {
                    self.bump();
                }
                self.builder.finish_node();
            }
            TokenKind::StrLiteral | TokenKind::CharLiteral => {
                if self.peek2_text() == "(" {
                    // Operator-symbol call: `"<"(a, b)` — parse as a name so
                    // the argument list becomes a Call suffix.
                    self.parse_name();
                } else {
                    self.start(SyntaxKind::Literal);
                    self.bump();
                    self.builder.finish_node();
                }
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
                    "x" | "b" | "o" | "d"
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
                // A range choice (`1 to N => ' '`) also makes this an aggregate.
                if self.at_kw("to") || self.at_kw("downto") {
                    self.bump();
                    self.parse_expression();
                }
                // Aggregate if the first element is followed by `,`, `|`
                // (more choices), `=>` (named association) or a range.
                let mut is_agg = self.peek_op2().as_deref() == Some("=>")
                    || self.peek_text() == "|"
                    || self.at_kw("to")
                    || self.at_kw("downto");
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
                        self.parse_agg_choice();
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
    #[test]
    fn parses_vhdl2008_parameter_keyword_and_sized_bitstrings() {
        let src = r#"package p is
    procedure check parameter (filename : string);
    function f return integer;
end package;
package body p is
    procedure check parameter (filename : string) is begin null; end procedure;
    function f return integer is begin return 32D"1" + 32SB"1" + 16X"ff"; end function;
end package body;
"#;
        let (p, db) = parse_str(src);
        no_errors(&p, &db);
    }

    fn parses_block_header_sized_bitstring_and_misc() {
        let src = r#"entity t is
    generic (g_w : natural := 32X"0" & "00");
end entity;
architecture a of t is
    signal d0, fz : bit := '0';
    disconnect bus_sig : bit after 3 ns;
    file f : text is "data.txt";
begin
    b : block is
            port ( z : out bit );
          port map ( z => fz );
    begin
        z <= d0;
    end block b;
    with d0 select (fz, d0) <= bit_vector'("00") when "0", "01" when others;
    p : alib.apkg.acomp port map (d0, fz);
    assert "<"(1, 2) = '1' report "no";
end architecture;
"#;
        let (p, db) = parse_str(src);
        no_errors(&p, &db);
        let sexp = p.root.debug_sexp(&db);
        assert!(sexp.contains("(PortClause"), "{}", sexp);
        assert!(sexp.contains("(DisconnectSpec"), "{}", sexp);
        assert!(sexp.contains("(FileDecl"), "{}", sexp);
        assert!(sexp.contains("(SelectedAssign"), "{}", sexp);
        assert!(sexp.contains("(Inst"), "{}", sexp);
        assert!(sexp.contains("(Call"), "{}", sexp); // "<"(1, 2)
                                                     // sized bit-string literal: size + base letter + string in one Literal
        assert!(
            sexp.contains("(Literal IntLiteral \"32\" Ident \"X\" StrLiteral"),
            "{}",
            sexp
        );
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
    fn parses_vhdl2008_wait_and_condition_operator() {
        // `wait on ... until ...` clause order, `??` condition operator, and a
        // parenthesized resolution function in a subtype declaration.
        let sexp = ok(r#"package p is
  subtype slv_max is (resolved_max) std_ulogic_vector;
end package p;
entity e is end entity;
architecture a of e is
  signal clk : bit := '0';
  signal en : boolean := true;
begin
  process
  begin
    wait on clk until (??(clk = '1')) and en;
    wait until ??en for 10 ns;
    wait;
  end process;
end architecture;
"#);
        assert!(sexp.contains("(Wait"), "{}", sexp);
        assert!(sexp.contains("(UnaryExpr"), "{}", sexp);
    }

    #[test]
    fn parses_for_loop_range_expressions() {
        // Attribute arithmetic in a for-loop discrete range (`input'length-1`)
        // and plain attribute bounds (`t'left to t'right`).
        let sexp = ok(r#"entity e is end entity;
architecture a of e is
  signal input : bit_vector(7 downto 0);
begin
  process
  begin
    for i in input'length-1 downto 0 loop
      null;
    end loop;
    for j in input'left to input'right loop
      null;
    end loop;
  end process;
end architecture;
"#);
        assert!(sexp.contains("(For"), "{}", sexp);
    }

    #[test]
    fn parses_vhdl2008_constructs() {
        // impure/pure functions, type generics + subprogram specs in a generic
        // clause, protected type + body, incomplete type, alias with [signature],
        // aggregate range choices, qualified aggregates, for-loop attribute
        // bounds, conditional expressions, parenthesized target list.
        let sexp = ok(r#"package p is
  generic (
    type ExpectedType;
    impure function Match(A : ExpectedType) return boolean
  );
  type ListType;
  type PType is protected
    procedure bump(x : in integer);
  end protected PType;
  alias F is g[integer, string];
end package;
package body p is
  type PType is protected body
    variable n : integer;
  end protected body PType;
  function f(a : integer_vector) return integer is
    variable v : integer;
  begin
    v := f(integer_vector'(1, 7)) when a'length > 0 else 0;
    for i in integer'left to integer'right loop
      v := v + i;
    end loop;
    return v;
  end function "+";
end package body;
architecture rtl of t is
  constant C : string := (1 to 4 => ' ');
  signal s : bit;
begin
  s <= '1' when C = "    " else '0';
  process
  begin
    (s, s) := f(integer_vector'(1, 2));
    wait;
  end process;
end architecture;
"#);
        assert!(sexp.contains("(GenericClause"), "{}", sexp);
        assert!(sexp.contains("(TypeDecl"), "{}", sexp);
        assert!(sexp.contains("(AliasDecl"), "{}", sexp);
        assert!(sexp.contains("(Qualified"), "{}", sexp);
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

    #[test]
    fn parses_context_declaration() {
        let sexp = ok(r#"library ieee;
context OsvvmContext is
  library OSVVM ;
  use OSVVM.IfElsePkg.all ;
end context OsvvmContext;
entity e is end entity;
"#);
        assert!(sexp.contains("(ContextDecl"), "{}", sexp);
    }

    #[test]
    fn parses_attribute_spec_with_signature() {
        let sexp = ok(r#"package p is
  impure function f(name: string) return integer;
  attribute foreign of f[string return integer]: function is "VHPI systf; f";
end package;
"#);
        assert!(sexp.contains("(AttrSpec"), "{}", sexp);
    }

    #[test]
    fn parses_multi_element_waveform_with_after() {
        let sexp = ok(r#"entity e is end entity;
architecture rtl of e is
  signal clk : bit := '0';
begin
  process
  begin
    clk <= '1', '0' after 5 ns, '1' after 10 ns;
    wait;
  end process;
end architecture;
"#);
        assert!(sexp.contains("(SeqAssign"), "{}", sexp);
    }

    #[test]
    fn parses_2019_generic_array_type_and_prefix_reduction() {
        let sexp = ok(r#"package p is
  generic (type VectorType is array (type is range <>) of type is private);
  subtype ElementType is VectorType'element;
end package;
package body p is
  function f(a : integer_vector) return bit is
  begin
    return (or a) and '1';
  end;
end package body;
"#);
        assert!(sexp.contains("Punct \"'\" Ident \"element\""), "{}", sexp);
        assert!(sexp.contains("(UnaryExpr Ident \"or\""), "{}", sexp);
    }

    #[test]
    fn parses_block_configurations_and_specifications() {
        let sexp = ok(r#"configuration cfg of top is
  for rtl
    for u0 : comp
      use entity work.other(sim) generic map (W => 4) port map (a => x);
    end for;
    for all : leaf use configuration work.leaf_cfg;
    end for;
    for others : leaf use entity work.dflt(rtl);
    end for;
  end for;
end configuration;
entity e is end entity;
architecture rtl of e is
  for u0 : comp use entity work.other(sim);
begin
end architecture;
"#);
        assert!(sexp.contains("(ConfigDecl"), "{}", sexp);
        assert!(sexp.matches("(UseClause").count() >= 3, "{}", sexp);
        assert!(sexp.matches("(ForSpec").count() >= 5, "{}", sexp);
    }

    #[test]
    fn instantiation_label_is_inside_the_inst_node() {
        let sexp = ok(r#"entity e is end entity;
architecture rtl of e is
begin
  u0 : entity work.dut(rtl) port map (a => x);
  u1 : configuration work.cfg port map (a => x);
end architecture;
"#);
        let insts: Vec<&str> = sexp.split("(Inst").skip(1).collect();
        assert_eq!(insts.len(), 2, "{}", sexp);
        assert!(
            insts[0].contains("Ident \"u0\" Punct \":\""),
            "{}",
            insts[0]
        );
        assert!(
            insts[1].contains("Ident \"u1\" Punct \":\""),
            "{}",
            insts[1]
        );
        assert!(insts[1].contains("Ident \"configuration\""), "{}", insts[1]);
    }

    #[test]
    fn parses_vhdl2008_conditional_expression() {
        // `when/else` in a pure expression position (here: an if-condition)
        // is a CondExpr; in a waveform it stays a flat conditional waveform.
        let sexp = ok(r#"entity e is end entity;
architecture rtl of e is
    signal v, y, a, b : bit;
begin
    y <= a when v = '1' else b;
    process(v)
    begin
        if (a when v = '1' else b) = '1' then
            y <= '1';
        end if;
    end process;
end architecture;
"#);
        let n_cond = sexp.matches("(CondExpr").count();
        assert_eq!(n_cond, 1, "{}", sexp);
    }

    #[test]
    fn parses_case_generate_labeled_alternative_bare_block() {
        // VHDL-2008: `when lbl : choice => begin ... end lbl;`
        let sexp = ok(r#"architecture rtl of e is
begin
    g4 : case conf generate
        when g4_1 : 1 | 2 =>
            begin
                cmp : configuration work.reg_conf1
                    generic map (width => 1)
                    port map (clk => clk, q => do);
            end g4_1;
        when others =>
    end generate g4;
end architecture;
"#);
        assert!(sexp.contains("(GenCase"), "{}", sexp);
        assert_eq!(sexp.matches("(CaseItem").count(), 2, "{}", sexp);
        assert!(sexp.contains("(Block"), "{}", sexp);
        assert!(sexp.contains("Ident \"configuration\""), "{}", sexp);
    }

    #[test]
    fn parses_alias_with_signature_and_sized_signed_bitstring() {
        let sexp = ok(r#"package p is
    type text_file is file of string;
    function is_eof (file t : text_file; fake : boolean) return boolean;
    alias iseof is is_eof [text_file, boolean return boolean];
    constant c : bit_vector := 32SB"1010";
end package;
"#);
        assert!(sexp.contains("(AliasDecl"), "{}", sexp);
        assert!(
            sexp.contains("(Literal IntLiteral \"32\" Ident \"SB\" StrLiteral"),
            "{}",
            sexp
        );
    }
}
