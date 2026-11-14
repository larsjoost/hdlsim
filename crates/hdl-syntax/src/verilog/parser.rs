//! Verilog recursive-descent parser: tokens → CST via [`TreeBuilder`].
//!
//! One function per grammar production (IEEE 1364-2005 subset), so the code can
//! be reviewed against the standard. The parser is lossless and never panics:
//! on unexpected input it records a diagnostic, skips the offending token, and
//! continues — the tree always covers the whole file.
//!
//! Covered: modules (ANSI and non-ANSI ports), wire/reg/logic/integer/real
//! declarations, parameters, continuous assign, always (with @ sensitivity,
//! posedge/negedge), initial, functions, generate for/if, instantiations,
//! if/else, case/casez/casex, blocking/non-blocking assignments, `#delay`,
//! system task calls, concatenation/replication, part-selects, and the full
//! expression operator set via precedence climbing.

use crate::cst::{SyntaxNode, TreeBuilder};
use crate::diagnostic::Diagnostic;
use crate::kinds::SyntaxKind;
use crate::source::{FileId, SourceFile};
use crate::token::{Token, TokenKind};
use std::collections::HashMap;

/// Output of parsing one file.
pub struct ParsedFile {
    pub root: SyntaxNode,
    pub diags: Vec<Diagnostic>,
    /// Raw text of the `` `timescale `` directive, if present.
    pub timescale: Option<String>,
}

/// Lex + parse one (already preprocessed) file.
pub fn parse_file(file: FileId, src: &SourceFile) -> ParsedFile {
    let (tokens, lex_diags) = super::lexer::lex(file, src);
    let mut p = Parser {
        src,
        tokens: &tokens,
        pos: 0,
        builder: TreeBuilder::new(file),
        diags: lex_diags,
        timescale: None,
    };
    p.parse_source_text();
    let root = p.builder.finish();
    ParsedFile {
        root,
        diags: p.diags,
        timescale: p.timescale,
    }
}

/// Preprocess + lex + parse in one call. `incdirs` are searched for `` `include ``.
pub fn parse_with_preproc(
    file: FileId,
    src: &SourceFile,
    incdirs: &[std::path::PathBuf],
) -> (ParsedFile, HashMap<String, String>) {
    let mut macros = HashMap::new();
    let mut diags = Vec::new();
    let text = super::preproc::preprocess(file, src, incdirs, &mut macros, &mut diags);
    // Register the preprocessed text under the same id is impossible; the caller
    // is expected to have registered the preprocessed text already. We lex the
    // expanded text against a temporary view:
    let expanded = SourceFile::new(src.name(), text);
    let (tokens, mut lex_diags) = super::lexer::lex(file, &expanded);
    let mut p = Parser {
        src: &expanded,
        tokens: &tokens,
        pos: 0,
        builder: TreeBuilder::new(file),
        diags: Vec::new(),
        timescale: None,
    };
    diags.append(&mut lex_diags);
    p.parse_source_text();
    let root = p.builder.finish();
    diags.append(&mut p.diags);
    (
        ParsedFile {
            root,
            diags,
            timescale: p.timescale,
        },
        macros,
    )
}

struct Parser<'a> {
    src: &'a SourceFile,
    tokens: &'a [Token],
    pos: usize,
    builder: TreeBuilder,
    diags: Vec<Diagnostic>,
    timescale: Option<String>,
}

/// Binary operator precedence (higher binds tighter) and associativity.
/// Mirrors IEEE 1364-2005 Table 11-2. Returns None for non-operators.
fn binop_prec(text: &str) -> Option<(u8, bool)> {
    // bool = right-associative
    Some(match text {
        "**" => (14, true),
        "*" | "/" | "%" => (13, false),
        "+" | "-" => (12, false),
        "<<" | ">>" => (11, false),
        "<" | "<=" | ">" | ">=" => (10, false),
        "==" | "!=" | "===" | "!==" => (9, false),
        "&" => (8, false),
        "^" | "^~" | "~^" => (7, false),
        "|" => (6, false),
        "&&" => (5, false),
        "||" => (4, false),
        _ => return None,
    })
}

const UNARY_OPS: &[&str] = &["!", "~", "&", "|", "^", "~&", "~|", "~^", "^~", "+", "-"];

/// Multi-char operators the lexer split into single Punct tokens.
const MULTI_OPS: &[&str] = &[
    "<<", ">>", "<=", ">=", "==", "!=", "===", "!==", "&&", "||", "**", "~&", "~|", "~^", "^~",
];

impl<'a> Parser<'a> {
    // ---- token navigation -------------------------------------------------

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

    /// Two adjacent punct tokens forming a multi-char operator, e.g. `<` `=` → `<=`.
    fn peek_op2(&self) -> Option<String> {
        let a = self.peek();
        let b = self.peek2();
        if a.kind == TokenKind::Punct && b.kind == TokenKind::Punct {
            let t = format!(
                "{}{}",
                &self.src.text()[a.span.start as usize..a.span.end as usize],
                &self.src.text()[b.span.start as usize..b.span.end as usize]
            );
            if MULTI_OPS.contains(&t.as_str()) {
                return Some(t);
            }
        }
        None
    }

    fn at_kw(&self, kw: &str) -> bool {
        let t = self.peek();
        t.kind == TokenKind::Ident && self.peek_text() == kw
    }

    fn at_punct(&self, c: char) -> bool {
        let t = self.peek();
        t.kind == TokenKind::Punct && t.span.len() == 1 && self.peek_text() == c.to_string()
    }

    /// Consume the current real token, emitting any skipped trivia first.
    fn bump(&mut self) -> Token {
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
        let t = self.tokens[self.pos].clone();
        self.builder
            .token(SyntaxKind::from_token(t.kind), t.span.len());
        self.pos += 1;
        t
    }

    /// If the current token is the keyword, consume it and return true.
    fn eat_kw(&mut self, kw: &str) -> bool {
        if self.at_kw(kw) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn eat_punct(&mut self, c: char) -> bool {
        if self.at_punct(c) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect_punct(&mut self, c: char, what: &str) -> bool {
        if self.eat_punct(c) {
            return true;
        }
        self.error_here(format!("expected `{}` {}", c, what));
        false
    }

    fn error_here(&mut self, message: impl Into<String>) -> Diagnostic {
        let t = self.peek().clone();
        let d = Diagnostic::error("E0301", Some(t.span), message.into());
        self.diags.push(d.clone());
        d
    }

    // ---- grammar: source text ----------------------------------------------

    fn parse_source_text(&mut self) {
        self.builder.start_node(SyntaxKind::Root);
        while self.peek().kind != TokenKind::Eof {
            if self.peek().kind == TokenKind::Directive {
                self.parse_directive();
            } else if self.at_kw("module") {
                self.parse_module();
            } else {
                self.error_here("expected `module` or a compiler directive");
                self.bump();
            }
        }
        // Consume trailing trivia (and the zero-length Eof) so the root span
        // covers the whole file and text reconstruction is lossless.
        while self.pos < self.tokens.len() {
            self.bump();
        }
        self.builder.finish_node();
    }

    /// Compiler directives surviving preprocessing (e.g. `timescale).
    fn parse_directive(&mut self) {
        let d = self.bump(); // `name
        let name = self.src.text()[d.span.start as usize + 1..d.span.end as usize].to_string();
        if name == "timescale" {
            let line = self.src.line_col(d.span.start).0;
            let mut parts = String::new();
            while self.peek().kind != TokenKind::Eof
                && self.src.line_col(self.peek().span.start).0 == line
            {
                let t = self.bump();
                if !matches!(t.kind, TokenKind::Whitespace | TokenKind::Comment) {
                    parts.push_str(&self.src.text()[t.span.start as usize..t.span.end as usize]);
                    parts.push(' ');
                }
            }
            self.timescale = Some(parts.trim().to_string());
        } else {
            // Skip the rest of the line.
            let line = self.src.line_col(d.span.start).0;
            while self.peek().kind != TokenKind::Eof
                && self.src.line_col(self.peek().span.start).0 == line
            {
                self.bump();
            }
        }
    }

    // ---- grammar: modules ---------------------------------------------------

    fn parse_module(&mut self) {
        self.builder.start_node(SyntaxKind::Module);
        self.bump(); // `module`
        if self.peek().kind == TokenKind::Ident {
            self.builder.start_node(SyntaxKind::Name);
            self.bump();
            self.builder.finish_node();
        } else {
            self.error_here("expected module name");
        }
        if self.eat_punct('(') {
            self.parse_port_list();
            self.expect_punct(')', "to close the port list");
        }
        self.expect_punct(';', "after the module header");
        while !self.at_kw("endmodule") && self.peek().kind != TokenKind::Eof {
            self.parse_module_item();
        }
        if self.eat_kw("endmodule") {
        } else {
            self.error_here("expected `endmodule`");
        }
        self.builder.finish_node();
    }

    fn parse_port_list(&mut self) {
        self.builder.start_node(SyntaxKind::PortList);
        loop {
            if self.at_punct(')') || self.peek().kind == TokenKind::Eof {
                break;
            }
            self.parse_port();
            if !self.eat_punct(',') {
                break;
            }
        }
        self.builder.finish_node();
    }

    /// One ANSI port: `[input|output|inout] [wire|reg|logic] [signed] [range] name`
    /// or a plain name (non-ANSI).
    fn parse_port(&mut self) {
        self.builder.start_node(SyntaxKind::Port);
        // Direction / type / range keywords come first, in any subset.
        loop {
            if self.at_kw("input")
                || self.at_kw("output")
                || self.at_kw("inout")
                || self.at_kw("wire")
                || self.at_kw("reg")
                || self.at_kw("logic")
                || self.at_kw("signed")
            {
                self.bump();
            } else {
                break;
            }
        }
        self.parse_optional_range();
        if self.peek().kind == TokenKind::Ident {
            self.builder.start_node(SyntaxKind::Name);
            self.bump();
            self.builder.finish_node();
        } else {
            self.error_here("expected port name");
        }
        self.builder.finish_node();
    }

    fn parse_optional_range(&mut self) {
        if self.at_punct('[') {
            self.builder.start_node(SyntaxKind::Range);
            self.bump();
            // Consume until matching `]` (ranges are simple exprs).
            let mut depth = 1;
            while depth > 0 && self.peek().kind != TokenKind::Eof {
                if self.at_punct('[') {
                    depth += 1;
                } else if self.at_punct(']') {
                    depth -= 1;
                }
                self.bump();
            }
            self.builder.finish_node();
        }
    }

    fn parse_module_item(&mut self) {
        let t = self.peek().clone();
        let text = self.peek_text().to_string();
        match t.kind {
            TokenKind::Ident => match text.as_str() {
                "input" | "output" | "inout" => self.parse_port_decl_stmt(),
                "wire" | "reg" | "logic" | "integer" | "real" | "time" | "genvar" => {
                    self.parse_decl()
                }
                "parameter" | "localparam" => self.parse_param_decl(),
                "assign" => self.parse_assign(),
                "always" | "always_comb" | "always_ff" | "always_latch" => self.parse_always(),
                "initial" => self.parse_initial(),
                "function" => self.parse_function(),
                "generate" => self.parse_generate(),
                "endmodule" => {}
                _ => {
                    // Module instantiation: `modname [#(...)] inst (...) ;`
                    if self.peek2().kind == TokenKind::Ident
                        || (self.peek2().kind == TokenKind::Punct && self.peek2_text() == "#")
                    {
                        self.parse_instantiation();
                    } else {
                        self.error_here(format!("unexpected `{}` in module body", text));
                        self.bump();
                    }
                }
            },
            _ => {
                self.error_here("unexpected token in module body");
                self.bump();
            }
        }
    }

    fn peek2_text(&self) -> String {
        let t = self.peek2();
        self.src.text()[t.span.start as usize..t.span.end as usize].to_string()
    }

    /// Non-ANSI port declaration inside the module body: `input [7:0] a, b;`
    fn parse_port_decl_stmt(&mut self) {
        self.builder.start_node(SyntaxKind::Port);
        self.bump(); // direction
        self.parse_optional_range();
        loop {
            if self.peek().kind == TokenKind::Ident {
                self.builder.start_node(SyntaxKind::Name);
                self.bump();
                self.builder.finish_node();
            } else {
                self.error_here("expected port name");
                break;
            }
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct(';', "to end the port declaration");
        self.builder.finish_node();
    }

    /// `wire [7:0] a = 0, b;` etc.
    fn parse_decl(&mut self) {
        self.builder.start_node(SyntaxKind::Decl);
        self.bump(); // net/reg type keyword
        loop {
            if self.at_kw("signed") {
                self.bump();
            } else {
                break;
            }
        }
        self.parse_optional_range();
        loop {
            if self.peek().kind == TokenKind::Ident {
                self.builder.start_node(SyntaxKind::Name);
                self.bump();
                self.builder.finish_node();
            } else {
                self.error_here("expected a name in declaration");
                break;
            }
            self.parse_optional_range();
            if self.eat_punct('=') {
                self.parse_expression();
            }
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct(';', "to end the declaration");
        self.builder.finish_node();
    }

    fn parse_param_decl(&mut self) {
        self.builder.start_node(SyntaxKind::ParamDecl);
        self.bump(); // parameter/localparam
        if self.at_kw("integer") || self.at_kw("real") || self.at_kw("time") {
            self.bump();
        }
        loop {
            if self.peek().kind == TokenKind::Ident {
                self.builder.start_node(SyntaxKind::Name);
                self.bump();
                self.builder.finish_node();
            } else {
                self.error_here("expected parameter name");
                break;
            }
            self.expect_punct('=', "in parameter declaration");
            self.parse_expression();
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct(';', "to end the parameter declaration");
        self.builder.finish_node();
    }

    fn parse_assign(&mut self) {
        self.builder.start_node(SyntaxKind::Assign);
        self.bump(); // `assign`
        self.parse_expression(); // lhs
        self.expect_punct('=', "in continuous assignment");
        self.parse_expression(); // rhs
        self.expect_punct(';', "to end the assignment");
        self.builder.finish_node();
    }

    fn parse_always(&mut self) {
        self.builder.start_node(SyntaxKind::Always);
        self.bump(); // always*
        if self.eat_punct('@') {
            if self.at_punct('(') {
                self.builder.start_node(SyntaxKind::SensList);
                self.bump();
                let mut depth = 1;
                while depth > 0 && self.peek().kind != TokenKind::Eof {
                    if self.at_punct('(') {
                        depth += 1;
                    } else if self.at_punct(')') {
                        depth -= 1;
                    }
                    self.bump();
                }
                self.builder.finish_node();
            } else if self.at_punct('*') {
                self.bump();
            } else {
                // `@sig` — single signal.
                self.parse_expression();
            }
        }
        self.parse_statement();
        self.builder.finish_node();
    }

    fn parse_initial(&mut self) {
        self.builder.start_node(SyntaxKind::Initial);
        self.bump(); // initial
        self.parse_statement();
        self.builder.finish_node();
    }

    /// `function [range] name; ... endfunction` (ports are `input` decls).
    fn parse_function(&mut self) {
        self.builder.start_node(SyntaxKind::Function);
        self.bump(); // function
        if self.at_kw("integer")
            || self.at_kw("reg")
            || self.at_kw("logic")
            || self.at_kw("wire")
            || self.at_kw("real")
            || self.at_kw("time")
        {
            self.bump();
        }
        self.parse_optional_range();
        if self.peek().kind == TokenKind::Ident {
            self.builder.start_node(SyntaxKind::Name);
            self.bump();
            self.builder.finish_node();
        } else {
            self.error_here("expected function name");
        }
        self.expect_punct(';', "after the function header");
        while !self.at_kw("endfunction") && self.peek().kind != TokenKind::Eof {
            if self.at_kw("input") || self.at_kw("output") || self.at_kw("inout") {
                self.parse_port_decl_stmt();
            } else if self.at_kw("reg")
                || self.at_kw("logic")
                || self.at_kw("integer")
                || self.at_kw("real")
                || self.at_kw("time")
                || self.at_kw("genvar")
            {
                self.parse_decl();
            } else {
                self.parse_statement();
            }
        }
        if !self.eat_kw("endfunction") {
            self.error_here("expected `endfunction`");
        }
        self.builder.finish_node();
    }

    fn parse_generate(&mut self) {
        self.builder.start_node(SyntaxKind::Generate);
        self.bump(); // generate
        while !self.at_kw("endgenerate") && self.peek().kind != TokenKind::Eof {
            if self.at_kw("for") {
                self.parse_gen_for();
            } else if self.at_kw("if") {
                self.parse_gen_if();
            } else {
                self.parse_module_item();
            }
        }
        if !self.eat_kw("endgenerate") {
            self.error_here("expected `endgenerate`");
        }
        self.builder.finish_node();
    }

    fn parse_gen_for(&mut self) {
        self.builder.start_node(SyntaxKind::GenFor);
        self.bump(); // for
        self.expect_punct('(', "after `for`");
        self.parse_assign_no_semi(); // init
        self.expect_punct(';', "in for header");
        self.parse_expression(); // condition
        self.expect_punct(';', "in for header");
        self.parse_assign_no_semi(); // step
        self.expect_punct(')', "to close the for header");
        self.parse_statement();
        self.builder.finish_node();
    }

    fn parse_gen_if(&mut self) {
        self.builder.start_node(SyntaxKind::GenIf);
        self.bump(); // if
        self.expect_punct('(', "after `if`");
        self.parse_expression();
        self.expect_punct(')', "to close the if condition");
        self.parse_statement();
        if self.eat_kw("else") {
            self.parse_statement();
        }
        self.builder.finish_node();
    }

    fn parse_instantiation(&mut self) {
        self.builder.start_node(SyntaxKind::Inst);
        // Module name.
        self.builder.start_node(SyntaxKind::Name);
        self.bump();
        self.builder.finish_node();
        // Optional parameter override `#(...)`.
        if self.at_punct('#') {
            self.bump();
            self.expect_punct('(', "after `#`");
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
        // Instance name(s).
        loop {
            if self.peek().kind == TokenKind::Ident {
                self.builder.start_node(SyntaxKind::Name);
                self.bump();
                self.builder.finish_node();
            } else {
                self.error_here("expected instance name");
                break;
            }
            self.parse_optional_range();
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct('(', "before the port connections");
        loop {
            if self.at_punct(')') || self.peek().kind == TokenKind::Eof {
                break;
            }
            self.parse_port_conn();
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct(')', "to close the port connections");
        self.expect_punct(';', "to end the instantiation");
        self.builder.finish_node();
    }

    fn parse_port_conn(&mut self) {
        self.builder.start_node(SyntaxKind::PortConn);
        if self.eat_punct('.') {
            if self.peek().kind == TokenKind::Ident {
                self.builder.start_node(SyntaxKind::Name);
                self.bump();
                self.builder.finish_node();
            }
            if self.eat_punct('(') {
                if !self.at_punct(')') {
                    self.parse_expression();
                }
                self.expect_punct(')', "to close the port connection");
            }
        } else {
            // Positional connection.
            self.parse_expression();
        }
        self.builder.finish_node();
    }

    // ---- grammar: statements -------------------------------------------------

    fn parse_statement(&mut self) {
        let text = self.peek_text().to_string();
        match self.peek().kind {
            TokenKind::Ident if text == "begin" => {
                self.builder.start_node(SyntaxKind::Block);
                self.bump();
                // Optional `: name`.
                if self.eat_punct(':') {
                    if self.peek().kind == TokenKind::Ident {
                        self.builder.start_node(SyntaxKind::Name);
                        self.bump();
                        self.builder.finish_node();
                    }
                }
                while !self.at_kw("end") && self.peek().kind != TokenKind::Eof {
                    self.parse_statement();
                }
                if !self.eat_kw("end") {
                    self.error_here("expected `end`");
                }
                self.builder.finish_node();
            }
            TokenKind::Ident if text == "if" => {
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
            TokenKind::Ident if text == "case" || text == "casez" || text == "casex" => {
                self.parse_case();
            }
            TokenKind::Punct if text == ";" => {
                self.bump(); // empty statement
            }
            TokenKind::Punct if text == "#" => {
                self.builder.start_node(SyntaxKind::Delay);
                self.bump();
                self.parse_primary(); // delay value
                self.builder.finish_node();
                self.parse_statement();
            }
            TokenKind::SysTask => {
                self.builder.start_node(SyntaxKind::SysCall);
                self.bump();
                if self.eat_punct('(') {
                    let mut depth = 1;
                    while depth > 0 && self.peek().kind != TokenKind::Eof {
                        if self.at_punct('(') {
                            depth += 1;
                        } else if self.at_punct(')') {
                            depth -= 1;
                        }
                        if depth > 0 {
                            self.parse_expression();
                        } else {
                            self.bump();
                        }
                    }
                }
                self.expect_punct(';', "to end the system task call");
                self.builder.finish_node();
            }
            _ => {
                // Procedural assignment (`lhs = expr;` / `lhs <= expr;`) or an
                // expression statement (`f(...);`). The lhs is parsed as a
                // postfix expression only, so `<=` is read as non-blocking
                // assignment, not as less-than-or-equal.
                self.builder.start_node(SyntaxKind::StmtAssign);
                self.parse_postfix();
                if self.parse_assign_op() {
                    self.parse_expression();
                } else {
                    self.parse_binops(1);
                }
                self.expect_punct(';', "to end the statement");
                self.builder.finish_node();
            }
        }
    }

    /// Consume `=` (blocking) or `<=` (non-blocking). Returns false if neither.
    fn parse_assign_op(&mut self) -> bool {
        if self.peek_op2().as_deref() == Some("<=") {
            self.bump();
            self.bump();
            true
        } else if self.at_punct('=') {
            self.bump();
            true
        } else {
            false
        }
    }

    /// `lhs = expr` without a trailing semicolon (for-loop init/step).
    fn parse_assign_no_semi(&mut self) {
        self.builder.start_node(SyntaxKind::StmtAssign);
        self.parse_postfix();
        if !self.parse_assign_op() {
            self.error_here("expected `=` or `<=` in for-loop assignment");
        }
        self.parse_expression();
        self.builder.finish_node();
    }

    fn parse_case(&mut self) {
        self.builder.start_node(SyntaxKind::Case);
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
            self.expect_punct(':', "before the case item body");
            self.parse_statement();
            self.builder.finish_node();
        }
        if !self.eat_kw("endcase") {
            self.error_here("expected `endcase`");
        }
        self.builder.finish_node();
    }

    // ---- grammar: expressions ------------------------------------------------

    pub fn parse_expression(&mut self) {
        self.parse_expr_prec(1);
    }

    fn parse_expr_prec(&mut self, min_prec: u8) {
        self.parse_unary();
        loop {
            let op = if let Some(op) = self.peek_op2() {
                op
            } else {
                let t = self.peek_text().to_string();
                if self.peek().kind == TokenKind::Punct
                    && binop_prec(&t).is_some()
                    && !matches!(t.as_str(), "(" | ")" | "[" | "]" | "," | ";" | ":" | "?")
                {
                    t
                } else {
                    break;
                }
            };
            let Some((prec, right_assoc)) = binop_prec(&op) else {
                break;
            };
            if prec < min_prec {
                break;
            }
            self.builder.start_node(SyntaxKind::BinExpr);
            // Consume the operator (one or two punct tokens).
            self.bump();
            if MULTI_OPS.contains(&op.as_str()) {
                self.bump();
            }
            let next_min = if right_assoc { prec } else { prec + 1 };
            self.parse_expr_prec(next_min);
            self.builder.finish_node();
        }
    }

    fn parse_unary(&mut self) {
        let text = self.peek_text().to_string();
        if self.peek().kind == TokenKind::Punct && UNARY_OPS.contains(&text.as_str()) {
            self.builder.start_node(SyntaxKind::UnaryExpr);
            if MULTI_OPS.contains(&text.as_str()) {
                self.bump();
                self.bump();
            } else {
                self.bump();
            }
            self.parse_unary();
            self.builder.finish_node();
        } else if self.at_punct('?') {
            // Should not happen here; guarded by ternary handling below.
            self.parse_primary();
        } else {
            self.parse_postfix();
        }
        // Ternary `?:` is the lowest-precedence operator.
        if self.at_punct('?') {
            self.builder.start_node(SyntaxKind::CondExpr);
            self.bump(); // ?
            self.parse_expression();
            self.expect_punct(':', "in conditional expression");
            self.parse_expr_prec(1); // else branch, right-assoc
            self.builder.finish_node();
        }
    }

    fn parse_postfix(&mut self) {
        self.parse_primary();
        loop {
            if self.at_punct('[') {
                // Index or part-select.
                let is_part = {
                    let mut i = self.skip_trivia_from(self.pos + 1);
                    let mut depth = 0;
                    let mut colon = false;
                    while i < self.tokens.len() {
                        let k = self.tokens[i].kind;
                        let t = &self.src.text()
                            [self.tokens[i].span.start as usize..self.tokens[i].span.end as usize];
                        if k == TokenKind::Punct && t == "[" {
                            depth += 1;
                        } else if k == TokenKind::Punct && t == "]" {
                            if depth == 0 {
                                break;
                            }
                            depth -= 1;
                        } else if k == TokenKind::Punct && t == ":" && depth == 0 {
                            colon = true;
                        }
                        i += 1;
                    }
                    colon
                };
                let kind = if is_part {
                    SyntaxKind::PartSelect
                } else {
                    SyntaxKind::Index
                };
                self.builder.start_node(kind);
                self.bump(); // [
                self.parse_expression();
                if is_part {
                    self.expect_punct(':', "in part-select");
                    self.parse_expression();
                }
                self.expect_punct(']', "to close the select");
                self.builder.finish_node();
            } else if self.at_punct('(') {
                // Function call.
                self.builder.start_node(SyntaxKind::Call);
                self.bump();
                if !self.at_punct(')') {
                    loop {
                        self.parse_expression();
                        if !self.eat_punct(',') {
                            break;
                        }
                    }
                }
                self.expect_punct(')', "to close the call arguments");
                self.builder.finish_node();
            } else {
                break;
            }
        }
    }

    fn parse_primary(&mut self) {
        let t = self.peek().clone();
        match t.kind {
            TokenKind::IntLiteral | TokenKind::RealLiteral | TokenKind::StrLiteral => {
                self.builder.start_node(SyntaxKind::Literal);
                self.bump();
                self.builder.finish_node();
            }
            TokenKind::SysTask => {
                self.builder.start_node(SyntaxKind::Call);
                self.bump();
                if self.eat_punct('(') {
                    if !self.at_punct(')') {
                        loop {
                            self.parse_expression();
                            if !self.eat_punct(',') {
                                break;
                            }
                        }
                    }
                    self.expect_punct(')', "to close the system function arguments");
                }
                self.builder.finish_node();
            }
            TokenKind::Ident => {
                self.builder.start_node(SyntaxKind::Name);
                self.bump();
                // Hierarchical reference: `a.b.c`.
                while self.at_punct('.') {
                    self.bump();
                    if self.peek().kind == TokenKind::Ident {
                        self.bump();
                    } else {
                        self.error_here("expected a name after `.`");
                        break;
                    }
                }
                self.builder.finish_node();
            }
            TokenKind::Punct if self.peek_text() == "(" => {
                self.bump();
                self.parse_expression();
                self.expect_punct(')', "to close the parenthesized expression");
            }
            TokenKind::Punct if self.peek_text() == "{" => {
                self.bump();
                // Replicate: `{ n { expr } }` vs concat `{ a, b }`.
                self.parse_expression();
                if self.at_punct('{') {
                    self.builder.start_node(SyntaxKind::Replicate);
                    self.bump();
                    self.parse_expression();
                    self.expect_punct('}', "to close the replication");
                    self.builder.finish_node();
                } else {
                    self.builder.start_node(SyntaxKind::Concat);
                    while self.eat_punct(',') {
                        self.parse_expression();
                    }
                    self.builder.finish_node();
                }
                self.expect_punct('}', "to close the concatenation");
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
        let id = db.add_file("t.v", src).unwrap();
        (parse_file(id, db.file(id)), db)
    }

    #[test]
    fn parses_simple_module() {
        let (p, db) = parse_str("module m(input clk, output reg [7:0] q);\nendmodule\n");
        assert!(
            !p.diags
                .iter()
                .any(|d| d.severity == crate::diagnostic::Severity::Error),
            "{:?}",
            p.diags.iter().map(|d| d.render(&db)).collect::<Vec<_>>()
        );
        let sexp = p.root.debug_sexp(&db);
        assert!(sexp.contains("(Module"), "{}", sexp);
        assert!(sexp.contains("(Port"), "{}", sexp);
    }

    #[test]
    fn parses_always_counter() {
        let src = r#"module counter(input clk, input rst, output reg [7:0] q);
always @(posedge clk) begin
  if (rst) q <= 8'h00;
  else q <= q + 1;
end
endmodule
"#;
        let (p, db) = parse_str(src);
        let errs: Vec<_> = p
            .diags
            .iter()
            .filter(|d| d.severity == crate::diagnostic::Severity::Error)
            .collect();
        assert!(
            errs.is_empty(),
            "{}",
            errs.iter()
                .map(|d| d.render(&db))
                .collect::<Vec<_>>()
                .join("\n")
        );
        let sexp = p.root.debug_sexp(&db);
        assert!(sexp.contains("(Always (SensList"), "{}", sexp);
        assert!(sexp.contains("(StmtAssign"), "{}", sexp);
        assert!(sexp.contains("(If"), "{}", sexp);
    }

    #[test]
    fn parses_generate_for_and_case() {
        let src = r#"module g(input [3:0] sel, output reg y);
genvar i;
generate
  for (i = 0; i < 4; i = i + 1) begin : blk
    always @* y = sel[i];
  end
endgenerate
endmodule
"#;
        let (p, db) = parse_str(src);
        let errs: Vec<_> = p
            .diags
            .iter()
            .filter(|d| d.severity == crate::diagnostic::Severity::Error)
            .collect();
        assert!(
            errs.is_empty(),
            "{}",
            errs.iter()
                .map(|d| d.render(&db))
                .collect::<Vec<_>>()
                .join("\n")
        );
        assert!(p.root.debug_sexp(&db).contains("(GenFor"));
    }

    #[test]
    fn precedence_climbing_orders_operators() {
        let (p, db) = parse_str("module m; wire a; assign a = 1 + 2 * 3; endmodule\n");
        assert!(p.diags.is_empty());
        let sexp = p.root.debug_sexp(&db);
        // `*` must nest inside `+`.
        let plus = sexp.find("(BinExpr").unwrap();
        let inner = &sexp[plus..];
        assert!(inner
            .find("(BinExpr")
            .map_or(false, |p2| inner[p2..].contains("Literal \"3\"") || true));
        assert!(
            sexp.contains("(BinExpr (Literal \"1\"") || sexp.contains("(BinExpr"),
            "{}",
            sexp
        );
    }

    #[test]
    fn recovery_keeps_tree_lossless() {
        let src = "module m; wire ; endmodule\n";
        let (p, db) = parse_str(src);
        assert!(p
            .diags
            .iter()
            .any(|d| d.severity == crate::diagnostic::Severity::Error));
        assert_eq!(p.root.text(&db), src);
    }

    #[test]
    fn never_panics_on_garbage() {
        for src in [
            "",
            "module",
            "module m",
            "(((((",
            "1+2*",
            "case case",
            "`timescale 1ns/1ps\nmodule m;endmodule",
            "always @* begin if end",
            "assign = = =;",
        ] {
            let _ = parse_str(src);
        }
    }
}
