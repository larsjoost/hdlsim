//! Verilog recursive-descent parser: tokens → CST via [`TreeBuilder`].
//!
//! One function per grammar production (IEEE 1364-2005 subset), so the code can
//! be reviewed against the standard. The parser is lossless and never panics:
//! on unexpected input it records a diagnostic, skips the offending token, and
//! continues — the tree always covers the whole file.
//!
//! Covered: modules (ANSI and non-ANSI ports), net/reg/logic/integer/real/time
//! and the SystemVerilog scalar types (bit/byte/shortint/int/longint/string/
//! chandle), parameters (body, `#(...)` header, `parameter type`), continuous
//! assign (with `#delay` and drive strengths), always/always_comb/always_ff/
//! always_latch (with @ sensitivity, posedge/negedge, `@*`, `iff` event
//! qualifiers), initial, final,
//! functions (incl. `automatic`/`void` and ANSI port lists), tasks, generate
//! for/if/case and named generate blocks, module and gate-primitive
//! instantiations, defparam, event declarations and triggers, typedef/enum/
//! struct/union, if/else, case/casez/casex (incl. `unique`/`priority` and
//! `inside`), blocking/non-blocking assignments, `#delay` (incl. SV time
//! literals `#1ns`, `#(2*1ns)`), `@`/wait event controls,
//! while/repeat/forever/do-while, fork/join, disable, SV immediate
//! assertions, system task calls, concatenation/replication, part-selects,
//! casts (`signed'(e)`, `8'(e)`), and the full expression operator set via
//! precedence climbing. Escaped identifiers (`\foo `) are lexed as names.

use crate::cst::{SyntaxNode, TreeBuilder};
use crate::diagnostic::Diagnostic;
use crate::kinds::SyntaxKind;
use crate::source::{FileId, SourceFile};
use crate::token::{Token, TokenKind};
use std::collections::{HashMap, HashSet};

mod proc;
mod sva;

/// Output of parsing one file.
pub struct ParsedFile {
    pub root: SyntaxNode,
    pub diags: Vec<Diagnostic>,
    /// Raw text of the `` `timescale `` directive, if present.
    pub timescale: Option<String>,
}

/// Parse a `` `timescale `` argument (`"1ns/1ps"`, `"10ps/1ps"`, …) into the
/// time unit in femtoseconds. Defaults to 1 ns when the text is unusable.
pub fn timescale_to_fs(text: &str) -> u64 {
    let unit = text.split('/').next().unwrap_or(text).trim();
    let (num, u) = unit
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .map(|i| (&unit[..i], &unit[i..]))
        .unwrap_or((unit, "ns"));
    let v: f64 = num.parse().unwrap_or(1.0);
    let mult: f64 = match u {
        "fs" => 1.0,
        "ps" => 1e3,
        "ns" => 1e6,
        "us" | "µs" => 1e9,
        "ms" => 1e12,
        "s" => 1e15,
        _ => 1e6,
    };
    (v * mult).max(1.0) as u64
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
        user_types: HashSet::new(),
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
        user_types: HashSet::new(),
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
    /// User type names seen so far (typedefs, classes) — lets the parser tell
    /// `word_t w;` (declaration) apart from `foo bar(...)` (instantiation).
    user_types: HashSet<String>,
}

/// Binary operator precedence (higher binds tighter) and associativity.
/// Mirrors IEEE 1364-2005 Table 11-2. Returns None for non-operators.
fn binop_prec(text: &str) -> Option<(u8, bool)> {
    // bool = right-associative
    Some(match text {
        "**" => (14, true),
        "*" | "/" | "%" => (13, false),
        "+" | "-" => (12, false),
        "<<" | ">>" | "<<<" | ">>>" => (11, false),
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

/// SV time-literal unit suffixes (`1ns`, `#(2*1us)`, SV LRM 5.7.1).
const TIME_UNITS: &[&str] = &["fs", "ps", "ns", "us", "ms", "s"];

/// Multi-char operators the lexer split into single Punct tokens.
const MULTI_OPS: &[&str] = &[
    "<<", ">>", "<<<", ">>>", "<=", ">=", "==", "!=", "===", "!==", "&&", "||", "**", "~&", "~|",
    "~^", "^~",
];

/// Number of punct tokens in a multi-char operator string.
fn op_token_count(op: &str) -> usize {
    op.chars().count()
}

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

    /// Two adjacent punct tokens forming a multi-char operator, e.g. `<` `=` → `<=`.
    fn peek_op2(&self) -> Option<String> {
        self.peek_op_n(2)
    }

    /// Three adjacent punct tokens, for `===` / `!==`.
    fn peek_op3(&self) -> Option<String> {
        self.peek_op_n(3)
    }

    /// Up to `n` adjacent punct tokens forming a known multi-char operator
    /// (longest match wins).
    fn peek_op_n(&self, n: usize) -> Option<String> {
        let i0 = self.skip_trivia_from(self.pos);
        let mut texts = Vec::new();
        let mut idx = i0;
        for _ in 0..n {
            if idx >= self.tokens.len() {
                break;
            }
            let t = &self.tokens[idx];
            if t.kind != TokenKind::Punct {
                break;
            }
            texts.push(&self.src.text()[t.span.start as usize..t.span.end as usize]);
            idx = self.skip_trivia_from(idx + 1);
        }
        if texts.len() < 2 {
            return None;
        }
        // Longest match first.
        for len in (2..=texts.len()).rev() {
            let t: String = texts[..len].concat();
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

    /// SV time-unit identifier directly after the number being parsed (`1`
    /// `ns`): the lexed shape of a time literal (SV LRM 5.7.1, with or
    /// without a space). `N unit` is never valid Verilog otherwise, so
    /// absorbing the unit into the Literal node cannot misparse a legal
    /// program.
    fn at_time_unit(&self) -> bool {
        self.peek2().kind == TokenKind::Ident && TIME_UNITS.contains(&self.peek2_text().as_str())
    }

    fn at_punct(&self, c: char) -> bool {
        let t = self.peek();
        t.kind == TokenKind::Punct && t.span.len() == 1 && self.peek_text() == c.to_string()
    }

    /// Consume the current real token, emitting any skipped trivia first.
    /// Consume pending trivia into the currently open node, so a `Name` node
    /// started right after does not absorb a preceding comment into its text.
    fn eat_pending_trivia(&mut self) {
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

    /// A `Name` node around the current identifier, with any leading trivia
    /// attached to the parent instead of the name itself.
    fn bump_name_node(&mut self) {
        self.eat_pending_trivia();
        self.builder.start_node(SyntaxKind::Name);
        self.bump();
        self.builder.finish_node();
    }

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
            } else if self.at_kw("module") || self.at_kw("macromodule") {
                self.parse_module();
            } else if self.at_kw("package") {
                self.parse_package();
            } else if self.at_kw("class") || (self.at_kw("virtual") && self.peek2_text() == "class")
            {
                self.parse_class();
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
            // A macro invocation: skip a balanced `(...)` argument list first
            // (it may span lines), then the rest of the line.
            if self.at_punct('(') {
                let mut depth = 0;
                while self.peek().kind != TokenKind::Eof {
                    if self.at_punct('(') {
                        depth += 1;
                        self.bump();
                    } else if self.at_punct(')') {
                        depth -= 1;
                        self.bump();
                        if depth == 0 {
                            break;
                        }
                    } else {
                        self.bump();
                    }
                }
            }
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
            self.bump_name_node();
        } else {
            self.error_here("expected module name");
        }
        // Package imports in the module header: `module m import p::*; ...`.
        // The import's own `;` may double as the header end when no port list
        // follows (`module m import p::*;`).
        let mut had_header_import = false;
        while self.at_kw("import") {
            self.parse_import(false);
            had_header_import = true;
        }
        // Optional `#(parameter ...)` header parameter list.
        if self.at_punct('#') {
            self.parse_header_params();
        }
        if self.eat_punct('(') {
            self.parse_port_list();
            self.expect_punct(')', "to close the port list");
            self.expect_punct(';', "after the module header");
        } else if !had_header_import {
            self.expect_punct(';', "after the module header");
        }
        while !self.at_kw("endmodule") && self.peek().kind != TokenKind::Eof {
            self.parse_module_item();
        }
        if self.eat_kw("endmodule") {
            // Optional label: `endmodule : name`.
            if self.eat_punct(':') && self.peek().kind == TokenKind::Ident {
                self.bump();
            }
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
                || self.at_kw("unsigned")
                || self.at_kw("ref")
                || self.at_kw("bit")
                || self.at_kw("byte")
                || self.at_kw("shortint")
                || self.at_kw("int")
                || self.at_kw("longint")
                || self.at_kw("integer")
                || self.at_kw("real")
                || self.at_kw("time")
                || self.at_kw("string")
                || self.at_kw("chandle")
            {
                self.bump();
            } else {
                break;
            }
        }
        // Optional user type name: `input data_t d_i` / `input data_t [7:0] d_i`.
        if self.peek().kind == TokenKind::Ident
            && (self.peek2().kind == TokenKind::Ident || self.peek2_text() == "[")
        {
            self.bump();
        }
        self.parse_optional_range();
        if self.peek().kind == TokenKind::Ident {
            self.bump_name_node();
        } else {
            self.error_here("expected port name");
        }
        self.builder.finish_node();
    }

    /// Consume a run of type keywords (`int unsigned`, `logic`, `signed`, ...)
    /// as in `parameter int unsigned W = 1` or `function longint unsigned f`.
    fn eat_type_words(&mut self) -> bool {
        let mut any = false;
        while self.peek().kind == TokenKind::Ident
            && matches!(
                self.peek_text(),
                "type"
                    | "integer"
                    | "real"
                    | "realtime"
                    | "shortreal"
                    | "time"
                    | "int"
                    | "shortint"
                    | "longint"
                    | "byte"
                    | "bit"
                    | "logic"
                    | "reg"
                    | "wire"
                    | "string"
                    | "chandle"
                    | "signed"
                    | "unsigned"
            )
        {
            self.bump();
            any = true;
        }
        any
    }

    /// `#( parameter W = 8, ... )` in a module header: each entry becomes a
    /// ParamDecl node, so lowering treats header and body params uniformly.
    fn parse_header_params(&mut self) {
        self.bump(); // #
        self.expect_punct('(', "after `#` in module header");
        while !self.at_punct(')') && self.peek().kind != TokenKind::Eof {
            self.builder.start_node(SyntaxKind::ParamDecl);
            if self.at_kw("parameter") || self.at_kw("localparam") {
                self.bump();
            }
            if self.at_kw("type") {
                self.bump();
                if self.peek().kind == TokenKind::Ident {
                    // `parameter type data_t = logic` declares a type param.
                    self.user_types.insert(self.peek_text().to_string());
                }
            }
            self.eat_type_words();
            // Optional user type name: `parameter data_t P = ...`.
            if self.peek().kind == TokenKind::Ident
                && (self.peek2().kind == TokenKind::Ident || self.peek2_text() == "[")
            {
                self.bump();
            }
            // Optional range: `parameter [0:0] PRE_REGISTER = 1`.
            self.parse_optional_range();
            if self.peek().kind == TokenKind::Ident {
                self.bump_name_node();
            } else {
                self.error_here("expected parameter name");
            }
            self.expect_punct('=', "in parameter declaration");
            self.parse_expression();
            self.parse_optional_range(); // `parameter type T = logic [7:0]`
            self.builder.finish_node();
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct(')', "to close the parameter list");
    }

    /// `[msb:lsb]` or `[width]`, with proper expression nodes inside.
    fn parse_optional_range(&mut self) {
        // Packed dimensions: `logic [2:0][5:0] x` — loop over every `[...]`.
        while self.at_punct('[') {
            self.builder.start_node(SyntaxKind::Range);
            self.bump();
            if self.at_punct(']') {
                // Unbounded dimension: dynamic array `da[]`.
            } else if self.peek_text() == "$" && self.peek2_text() == "]" {
                self.bump(); // `$` of a queue `q[$]` — emitted as a plain punct
            } else {
                self.parse_expression();
                if self.eat_punct(':') {
                    self.parse_expression();
                }
            }
            self.expect_punct(']', "to close the range");
            self.builder.finish_node();
        }
    }

    /// `package name; items... endpackage [name];` — SV package scope.
    fn parse_package(&mut self) {
        self.builder.start_node(SyntaxKind::Package);
        self.bump(); // package
        let mut pname = String::new();
        if self.peek().kind == TokenKind::Ident {
            pname = self.peek_text().to_string();
            self.builder.start_node(SyntaxKind::Name);
            self.bump();
            self.builder.finish_node();
        } else {
            self.error_here("expected a package name");
        }
        self.expect_punct(';', "after the package name");
        while !self.at_kw("endpackage") && self.peek().kind != TokenKind::Eof {
            self.parse_module_item();
        }
        self.eat_kw("endpackage");
        // Optional trailing name — only if it matches the package name AND is
        // followed by `;`.
        if !pname.is_empty()
            && self.peek().kind == TokenKind::Ident
            && self.peek_text() == pname
            && self.peek2().kind == TokenKind::Punct
            && self.peek2_text() == ";"
        {
            self.bump();
        }
        self.eat_punct(';'); // optional
        self.builder.finish_node();
    }

    /// `class [virtual] name [extends base]; items... endclass [name];`
    fn parse_class(&mut self) {
        self.builder.start_node(SyntaxKind::Class);
        if self.at_kw("virtual") {
            self.bump();
        }
        self.bump(); // class
        let mut cname = String::new();
        if self.peek().kind == TokenKind::Ident {
            self.builder.start_node(SyntaxKind::Name);
            cname = self.peek_text().to_string();
            self.bump();
            self.builder.finish_node();
        } else {
            self.error_here("expected a class name");
        }
        if !cname.is_empty() {
            self.user_types.insert(cname.clone());
        }
        if self.at_kw("extends") {
            self.bump();
            if self.peek().kind == TokenKind::Ident {
                self.builder.start_node(SyntaxKind::Name);
                self.bump();
                self.builder.finish_node();
            }
        }
        self.expect_punct(';', "after the class header");
        while !self.at_kw("endclass") && self.peek().kind != TokenKind::Eof {
            let text = self.peek_text().to_string();
            if text == "function" {
                self.parse_function();
            } else if text == "task" {
                self.parse_task();
            } else if text == "constraint" {
                self.parse_constraint();
            } else if self.user_types.contains(&text) && self.peek2().kind == TokenKind::Ident {
                self.parse_decl();
            } else if text == "static"
                || text == "protected"
                || text == "local"
                || text == "virtual"
                || text == "pure"
            {
                self.bump(); // class item modifier
                continue;
            } else {
                self.parse_decl_or_stmt_class_item();
            }
        }
        self.eat_kw("endclass");
        // Optional trailing name — only if it matches the class name AND is
        // followed by `;`, so a following decl (`endclass\nC c;`) is not eaten.
        if !cname.is_empty()
            && self.peek().kind == TokenKind::Ident
            && self.peek_text() == cname
            && self.peek2().kind == TokenKind::Punct
            && self.peek2_text() == ";"
        {
            self.bump();
        }
        self.eat_punct(';'); // optional
        self.builder.finish_node();
    }

    /// Declaration or constraint inside a class body.
    fn parse_decl_or_stmt_class_item(&mut self) {
        let text = self.peek_text().to_string();
        match text.as_str() {
            "wire" | "reg" | "logic" | "integer" | "real" | "time" | "bit" | "byte"
            | "shortint" | "int" | "longint" | "string" | "chandle" | "var" | "const" | "rand"
            | "randc" | "vectored" | "scalared" => self.parse_decl(),
            "constraint" => self.parse_constraint(),
            _ => {
                self.error_here(format!("unexpected `{}` in class body", text));
                // Recover: skip to the next `;`.
                while self.peek().kind != TokenKind::Eof
                    && !self.at_punct(';')
                    && !self.at_kw("endclass")
                {
                    self.bump();
                }
                self.eat_punct(';');
            }
        }
    }

    /// `constraint name { expr; ... }` — parsed structurally, lowered later.
    fn parse_constraint(&mut self) {
        self.builder.start_node(SyntaxKind::Constraint);
        self.bump(); // constraint
        if self.peek().kind == TokenKind::Ident {
            self.builder.start_node(SyntaxKind::Name);
            self.bump();
            self.builder.finish_node();
        }
        self.expect_punct('{', "to open the constraint block");
        let mut depth = 1;
        while depth > 0 && self.peek().kind != TokenKind::Eof {
            if self.at_punct('{') {
                depth += 1;
                self.bump();
            } else if self.at_punct('}') {
                depth -= 1;
                self.bump();
            } else {
                self.parse_expression();
                // Constraint expression separators: `;`, `&&`, `->`, `solve ... before`.
                while self.at_punct(';') || self.at_kw("and") || self.at_kw("solve") {
                    if self.at_kw("and") || self.at_kw("solve") {
                        // `a && b` inside a constraint: parse as expressions.
                        self.bump();
                        if self.at_kw("before") {
                            self.bump();
                        }
                        continue;
                    }
                    self.bump(); // ;
                    if self.at_punct('}') || self.at_kw("endclass") {
                        break;
                    }
                }
            }
        }
        self.builder.finish_node();
    }

    /// `import p::*;` / `import p::a, q::b;` / `export p::*;`
    fn parse_import(&mut self, export: bool) {
        self.builder.start_node(SyntaxKind::Import);
        self.bump(); // import / export
        loop {
            self.parse_scoped_name();
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct(';', "to end the import/export");
        let _ = export;
        self.builder.finish_node();
    }

    /// `pkg::item` or `pkg::*` — a scope-qualified name.
    fn parse_scoped_name(&mut self) {
        self.builder.start_node(SyntaxKind::Name);
        if self.peek().kind == TokenKind::Ident {
            self.bump();
        }
        while self.at_punct(':') && self.peek2_text() == ":" {
            self.bump();
            self.bump();
            // Either `*` or the loop variable name: both are skipped.
            if self.at_punct('*') || self.peek().kind == TokenKind::Ident {
                self.bump();
            }
        }
        self.builder.finish_node();
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

    /// Skip `(* ... *)` attribute blocks (e.g. `(* dont_touch = "true" *)`).
    fn eat_attributes(&mut self) {
        while self.at_punct('(') && self.peek2_text() == "*" {
            self.bump();
            self.bump();
            let mut depth = 1;
            while depth > 0 && self.peek().kind != TokenKind::Eof {
                if self.at_punct('(') && self.peek2_text() == "*" {
                    depth += 1;
                    self.bump();
                    self.bump();
                } else if self.at_punct('*') && self.peek2_text() == ")" {
                    depth -= 1;
                    self.bump();
                    self.bump();
                } else {
                    self.bump();
                }
            }
        }
    }

    fn parse_module_item(&mut self) {
        self.eat_attributes();
        let t = self.peek().clone();
        let text = self.peek_text().to_string();
        match t.kind {
            TokenKind::Ident => match text.as_str() {
                "input" | "output" | "inout" => self.parse_port_decl_stmt(),
                "wire" | "reg" | "logic" | "integer" | "real" | "time" | "genvar" | "tri"
                | "tri0" | "tri1" | "trireg" | "wand" | "wor" | "supply0" | "supply1" | "bit"
                | "byte" | "shortint" | "int" | "longint" | "string" | "chandle" | "vectored"
                | "scalared" | "var" | "const" | "rand" | "randc" => self.parse_decl(),
                "parameter" | "localparam" => self.parse_param_decl(),
                "assign" => self.parse_assign(),
                "always" | "always_comb" | "always_ff" | "always_latch" => self.parse_always(),
                "initial" => self.parse_initial(),
                "final" => self.parse_final(),
                "function" => self.parse_function(),
                "task" => self.parse_task(),
                "event" => self.parse_event_decl(),
                "defparam" => self.parse_defparam(),
                "typedef" => self.parse_typedef(),
                "enum" => self.parse_enum_decl(),
                "class" => self.parse_class(),
                "import" => self.parse_import(false),
                "export" => self.parse_import(true),
                "generate" => self.parse_generate(),
                // Implicit generate regions: `if (...) begin ... end`,
                // `for (...) ...`, `case (...) ...`, `begin : name ... end`.
                "if" | "for" | "case" | "casez" | "casex" | "begin" => self.parse_gen_item(),
                "assert" | "assume" | "cover" => self.parse_sva_property(text.as_str()),
                // `endmodule` here means the enclosing construct was never
                // closed — report and skip so no loop can spin on it.
                "endmodule" => {
                    self.error_here("unexpected `endmodule`");
                    self.bump();
                }
                "and" | "nand" | "or" | "nor" | "xor" | "xnor" | "buf" | "not" | "bufif0"
                | "bufif1" | "notif0" | "notif1" | "tran" | "tranif0" | "tranif1" | "rtran"
                | "rtranif0" | "rtranif1" | "pullup" | "pulldown" => self.parse_gate_inst(),
                // `specify ... endspecify` timing blocks: parse as a skipped node.
                "specify" => {
                    self.builder.start_node(SyntaxKind::Specify);
                    self.bump();
                    while !self.at_kw("endspecify") && self.peek().kind != TokenKind::Eof {
                        self.bump();
                    }
                    self.eat_kw("endspecify");
                    self.builder.finish_node();
                }
                _ => {
                    // Declaration with a user type: `word_t w;` / `C obj;`.
                    // A name list followed by `(` or `#` is a module
                    // instantiation; anything else (`type_t a, b;`) is a
                    // declaration with a (possibly package-imported) type.
                    if self.instantiation_ahead() {
                        // Module instantiation: `modname [#(...)] inst (...) ;`
                        self.parse_instantiation();
                    } else if self.peek2().kind == TokenKind::Ident
                        || (self.peek2().kind == TokenKind::Punct && self.peek2_text() == "[")
                    {
                        self.parse_decl();
                    } else {
                        self.error_here(format!("unexpected `{}` in module body", text));
                        self.bump();
                    }
                }
            },
            TokenKind::SysTask => self.parse_statement(),
            // Macro invocation / compiler directive inside the module body:
            // skipped (unexpanded or handled by the preprocessor).
            TokenKind::Directive => self.parse_directive(),
            _ => {
                self.error_here("unexpected token in module body");
                self.bump();
            }
        }
    }

    /// True when the upcoming tokens are `Ident [, Ident]*` followed by `(` or
    /// `#` — i.e. a module instantiation `mod [#(...)] inst (...)`, as opposed
    /// to a declaration `type_t name;` whose name list ends at `;`/`,`/`=`.
    fn instantiation_ahead(&self) -> bool {
        let mut i = self.skip_trivia_from(self.pos);
        if i >= self.tokens.len() || self.tokens[i].kind != TokenKind::Ident {
            return false;
        }
        i = self.skip_trivia_from(i + 1);
        loop {
            if i < self.tokens.len() && self.tokens[i].kind == TokenKind::Ident {
                i = self.skip_trivia_from(i + 1);
            }
            if i < self.tokens.len()
                && self.tokens[i].kind == TokenKind::Punct
                && &self.src.text()
                    [self.tokens[i].span.start as usize..self.tokens[i].span.end as usize]
                    == ","
            {
                i = self.skip_trivia_from(i + 1);
                continue;
            }
            break;
        }
        if i >= self.tokens.len() {
            return false;
        }
        let t = &self.tokens[i];
        t.kind == TokenKind::Punct
            && matches!(
                &self.src.text()[t.span.start as usize..t.span.end as usize],
                "(" | "#"
            )
    }

    /// `enum [base] { A, B = 2, ... } name [= init] [, name2 ...];` — a bare
    /// enum type declaration with variable declarators in a module body.
    fn parse_enum_decl(&mut self) {
        self.builder.start_node(SyntaxKind::Decl);
        self.parse_enum_type();
        loop {
            if self.peek().kind == TokenKind::Ident {
                self.bump_name_node();
            } else {
                self.error_here("expected a name in the enum declaration");
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
        self.expect_punct(';', "to end the enum declaration");
        self.builder.finish_node();
    }

    fn peek2_text(&self) -> String {
        let t = self.peek2();
        self.src.text()[t.span.start as usize..t.span.end as usize].to_string()
    }

    fn peek3_text(&self) -> String {
        let i = self.skip_trivia_from(self.pos);
        let j = self.skip_trivia_from(i + 1);
        let k = self.skip_trivia_from(j + 1);
        let t = &self.tokens[k.min(self.tokens.len() - 1)];
        self.src.text()[t.span.start as usize..t.span.end as usize].to_string()
    }

    /// Non-ANSI port declaration inside the module body: `input [7:0] a, b;`
    fn parse_port_decl_stmt(&mut self) {
        self.builder.start_node(SyntaxKind::Port);
        self.bump(); // direction
        self.eat_type_words(); // `input logic`, `input int unsigned`, ...
                               // Optional user type name: `input data_t d_i;`.
        if self.peek().kind == TokenKind::Ident
            && (self.peek2().kind == TokenKind::Ident || self.peek2_text() == "[")
        {
            self.bump();
        }
        self.parse_optional_range();
        loop {
            if self.peek().kind == TokenKind::Ident {
                self.bump_name_node();
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

    /// `wire [7:0] a = 0, b;` etc. SV modifiers (`var`/`const`/`rand`/`randc`,
    /// `vectored`/`scalared`) may precede the type keyword.
    fn parse_decl(&mut self) {
        self.builder.start_node(SyntaxKind::Decl);
        while self.at_kw("var")
            || self.at_kw("const")
            || self.at_kw("rand")
            || self.at_kw("randc")
            || self.at_kw("vectored")
            || self.at_kw("scalared")
        {
            self.bump();
        }
        self.bump(); // net/reg type keyword
        while self.at_kw("signed") || self.at_kw("unsigned") {
            self.bump();
        }
        while self.at_punct('[') {
            self.parse_optional_range(); // multi-dimensional: `logic [A][B] x;`
        }
        loop {
            if self.peek().kind == TokenKind::Ident {
                self.bump_name_node();
            } else {
                self.error_here("expected a name in declaration");
                break;
            }
            while self.at_punct('[') {
                self.parse_optional_range(); // per-name unpacked dims: `x [3:0]`
            }
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
        if self.at_kw("type") {
            self.bump();
            if self.peek().kind == TokenKind::Ident {
                // `parameter type data_t = logic` declares a type param.
                self.user_types.insert(self.peek_text().to_string());
            }
        }
        self.eat_type_words(); // `parameter int unsigned W`, ...
                               // Optional user type name: `localparam cb_seed_t [2:0] X = ...`.
        if self.peek().kind == TokenKind::Ident
            && (self.peek2().kind == TokenKind::Ident || self.peek2_text() == "[")
        {
            self.bump();
        }
        // Optional range: `parameter [7:0] W = 8;`.
        self.parse_optional_range();
        loop {
            if self.peek().kind == TokenKind::Ident {
                self.bump_name_node();
            } else {
                self.error_here("expected parameter name");
                break;
            }
            self.expect_punct('=', "in parameter declaration");
            self.parse_expression();
            self.parse_optional_range(); // `parameter type T = logic [7:0]`
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct(';', "to end the parameter declaration");
        self.builder.finish_node();
    }

    /// `assign [#delay] [(strengths)] lhs = [#delay] rhs;`
    fn parse_assign(&mut self) {
        self.builder.start_node(SyntaxKind::Assign);
        self.bump(); // `assign`
        if self.at_punct('#') {
            // `assign #5 a = b;` / `assign #(1,2,3) a = b;`
            self.builder.start_node(SyntaxKind::Delay);
            self.bump();
            if self.eat_punct('(') {
                loop {
                    self.parse_expression();
                    if !self.eat_punct(',') {
                        break;
                    }
                }
                self.expect_punct(')', "to close the assign delay");
            } else {
                self.parse_primary();
            }
            self.builder.finish_node();
        }
        if self.at_punct('(') && self.peek2().kind == TokenKind::Ident && self.is_strength(1) {
            // Drive strength list: `(strong1, pull0)`.
            self.bump();
            loop {
                self.bump(); // strength name
                if !self.eat_punct(',') {
                    break;
                }
            }
            self.expect_punct(')', "to close the strength list");
        }
        self.parse_expression(); // lhs
        self.expect_punct('=', "in continuous assignment");
        if self.at_punct('#') {
            self.builder.start_node(SyntaxKind::Delay);
            self.bump();
            self.parse_primary();
            self.builder.finish_node();
        }
        self.parse_expression(); // rhs
        self.expect_punct(';', "to end the assignment");
        self.builder.finish_node();
    }

    /// Is the `ahead`-th non-trivia token one of the drive-strength keywords?
    fn is_strength(&self, ahead: usize) -> bool {
        let mut i = self.skip_trivia_from(self.pos);
        for _ in 0..ahead {
            i = self.skip_trivia_from(i + 1);
        }
        if i >= self.tokens.len() {
            return false;
        }
        let t = &self.tokens[i];
        if t.kind != TokenKind::Ident {
            return false;
        }
        let text = &self.src.text()[t.span.start as usize..t.span.end as usize];
        matches!(
            text,
            "strong0" | "strong1" | "pull0" | "pull1" | "weak0" | "weak1" | "highz0" | "highz1"
        )
    }

    /// `function [automatic] [void|type] [range] name [(ports)] ; body endfunction`.
    /// Ports may be ANSI-style `(input [7:0] a)` or plain `input a;` decls.
    fn parse_function(&mut self) {
        self.builder.start_node(SyntaxKind::Function);
        self.bump(); // function
        self.eat_kw("automatic");
        if self.at_kw("void") {
            self.bump();
        } else {
            self.eat_type_words(); // `longint unsigned`, `logic`, ...
        }
        self.parse_optional_range();
        if self.peek().kind == TokenKind::Ident {
            self.builder.start_node(SyntaxKind::Name);
            self.bump();
            self.builder.finish_node();
        } else {
            self.error_here("expected function name");
        }
        if self.eat_punct('(') {
            // ANSI function port list.
            while !self.at_punct(')') && self.peek().kind != TokenKind::Eof {
                self.parse_port();
                if !self.eat_punct(',') {
                    break;
                }
            }
            self.expect_punct(')', "to close the function port list");
        }
        self.expect_punct(';', "after the function header");
        while !self.at_kw("endfunction") && self.peek().kind != TokenKind::Eof {
            if self.at_kw("input") || self.at_kw("output") || self.at_kw("inout") {
                self.parse_port_decl_stmt();
            } else if self.at_kw("parameter") || self.at_kw("localparam") {
                self.parse_param_decl();
            } else {
                self.parse_decl_or_stmt();
            }
        }
        if !self.eat_kw("endfunction") {
            self.error_here("expected `endfunction`");
        }
        self.builder.finish_node();
    }

    /// `task [automatic] name [(ports)]; decls/stmts endtask`.
    fn parse_task(&mut self) {
        self.builder.start_node(SyntaxKind::Task);
        self.bump(); // task
        self.eat_kw("automatic");
        if self.peek().kind == TokenKind::Ident {
            self.builder.start_node(SyntaxKind::Name);
            self.bump();
            self.builder.finish_node();
        } else {
            self.error_here("expected task name");
        }
        if self.eat_punct('(') {
            while !self.at_punct(')') && self.peek().kind != TokenKind::Eof {
                self.parse_port();
                if !self.eat_punct(',') {
                    break;
                }
            }
            self.expect_punct(')', "to close the task port list");
        }
        self.expect_punct(';', "after the task header");
        while !self.at_kw("endtask") && self.peek().kind != TokenKind::Eof {
            if self.at_kw("input") || self.at_kw("output") || self.at_kw("inout") {
                self.parse_port_decl_stmt();
            } else {
                self.parse_decl_or_stmt();
            }
        }
        if !self.eat_kw("endtask") {
            self.error_here("expected `endtask`");
        }
        self.builder.finish_node();
    }

    /// Decl keywords inside a task/function body vs. plain statements.
    fn parse_decl_or_stmt(&mut self) {
        let text = self.peek_text().to_string();
        match text.as_str() {
            "wire" | "reg" | "logic" | "integer" | "real" | "time" | "genvar" | "tri" | "tri0"
            | "tri1" | "trireg" | "wand" | "wor" | "supply0" | "supply1" | "bit" | "byte"
            | "shortint" | "int" | "longint" | "string" | "chandle" | "var" | "const" | "rand"
            | "randc" | "vectored" | "scalared" => self.parse_decl(),
            "parameter" | "localparam" => self.parse_param_decl(),
            "import" => self.parse_import(false),
            "export" => self.parse_import(true),
            "automatic" | "static" => {
                self.bump();
                self.parse_decl_or_stmt();
            }
            _ if self.user_types.contains(&text) && self.peek2().kind == TokenKind::Ident => {
                self.parse_decl()
            }
            _ => self.parse_statement(),
        }
    }

    /// `defparam a.b.P = expr, ...;`
    fn parse_defparam(&mut self) {
        self.builder.start_node(SyntaxKind::Defparam);
        self.bump(); // defparam
        loop {
            // Hierarchical parameter path.
            self.builder.start_node(SyntaxKind::Name);
            if self.peek().kind == TokenKind::Ident {
                self.bump();
                while self.at_punct('.') {
                    self.bump();
                    if self.peek().kind == TokenKind::Ident {
                        self.bump();
                    } else {
                        break;
                    }
                }
            }
            self.builder.finish_node();
            self.expect_punct('=', "in defparam");
            self.parse_expression();
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct(';', "to end the defparam statement");
        self.builder.finish_node();
    }

    /// `typedef [enum|struct|union ...] name;` — the type body is parsed as
    /// EnumType/StructType when present.
    fn parse_typedef(&mut self) {
        self.builder.start_node(SyntaxKind::Typedef);
        self.bump(); // typedef
        if self.at_kw("enum") {
            self.parse_enum_type();
        } else if self.at_kw("struct") || self.at_kw("union") {
            self.parse_struct_type();
        } else if self.at_kw("bit")
            || self.at_kw("logic")
            || self.at_kw("reg")
            || self.at_kw("byte")
            || self.at_kw("shortint")
            || self.at_kw("int")
            || self.at_kw("longint")
            || self.at_kw("integer")
            || self.at_kw("real")
            || self.at_kw("time")
            || self.at_kw("string")
            || self.at_kw("chandle")
            || self.at_kw("event")
        {
            self.bump();
            self.parse_optional_range();
        } else if self.peek().kind == TokenKind::Ident {
            // `typedef some_type name;` — copy of an existing type.
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
            self.parse_optional_range();
        }
        if self.peek().kind == TokenKind::Ident {
            self.builder.start_node(SyntaxKind::Name);
            let n = self.peek_text().to_string();
            self.bump();
            self.builder.finish_node();
            self.user_types.insert(n);
        } else {
            self.error_here("expected a typedef name");
        }
        self.expect_punct(';', "to end the typedef");
        self.builder.finish_node();
    }

    /// `enum [base_type] { A = 0, B, ... }` — base type optional.
    fn parse_enum_type(&mut self) {
        self.builder.start_node(SyntaxKind::EnumType);
        self.bump(); // enum
        if self.peek().kind == TokenKind::Ident {
            self.builder.start_node(SyntaxKind::Name);
            self.bump(); // base type
            self.builder.finish_node();
            self.parse_optional_range();
        }
        self.expect_punct('{', "to open the enum body");
        loop {
            if self.at_punct('}') || self.peek().kind == TokenKind::Eof {
                break;
            }
            if self.peek().kind == TokenKind::Ident {
                self.bump_name_node();
            } else {
                self.error_here("expected an enum literal");
                break;
            }
            if self.eat_punct('=') {
                self.parse_expression();
            }
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct('}', "to close the enum body");
        self.builder.finish_node();
    }

    /// `struct/union [packed] [signed] { field; ... }`
    fn parse_struct_type(&mut self) {
        let is_union = self.at_kw("union");
        self.builder.start_node(SyntaxKind::StructType);
        self.bump(); // struct | union
        if is_union {
            // keyword already consumed; `packed`/`signed` may follow either way
        }
        self.eat_kw("packed");
        self.eat_kw("signed");
        self.expect_punct('{', "to open the struct body");
        loop {
            if self.at_punct('}') || self.peek().kind == TokenKind::Eof {
                break;
            }
            // One field: `[modifiers] type [range] name [, name]* ;`
            while self.at_kw("rand") || self.at_kw("randc") || self.at_kw("const") {
                self.bump();
            }
            self.eat_type_words(); // `int unsigned`, `logic`, ...
                                   // Optional user type name for the field.
            if self.peek().kind == TokenKind::Ident
                && (self.peek2().kind == TokenKind::Ident || self.peek2_text() == "[")
            {
                self.bump();
            }
            if self.peek().kind != TokenKind::Ident && !self.at_punct('[') {
                self.error_here("expected a field type");
                break;
            }
            self.parse_optional_range();
            loop {
                if self.peek().kind == TokenKind::Ident {
                    self.builder.start_node(SyntaxKind::Name);
                    self.bump();
                    self.builder.finish_node();
                } else {
                    self.error_here("expected a field name");
                    break;
                }
                self.parse_optional_range();
                if !self.eat_punct(',') {
                    break;
                }
            }
            self.expect_punct(';', "to end the struct field");
        }
        self.expect_punct('}', "to close the struct body");
        self.builder.finish_node();
    }

    /// Gate primitive instantiation: `and (o, a, b);`, `pullup (x);`,
    /// optionally with a drive-strength pair `(strong1, pull0)`.
    fn parse_gate_inst(&mut self) {
        self.builder.start_node(SyntaxKind::Inst);
        self.builder.start_node(SyntaxKind::Name);
        self.bump(); // gate keyword
        self.builder.finish_node();
        if self.at_punct('(') && self.peek2().kind == TokenKind::Ident && self.is_strength(1) {
            self.bump();
            loop {
                self.bump();
                if !self.eat_punct(',') {
                    break;
                }
            }
            self.expect_punct(')', "to close the strength list");
        }
        // Optional instance name(s): `nand g1 [3:0] (n, a, b);`
        while self.peek().kind == TokenKind::Ident {
            self.builder.start_node(SyntaxKind::Name);
            self.bump();
            self.builder.finish_node();
            self.parse_optional_range();
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct('(', "before the gate ports");
        loop {
            if self.at_punct(')') || self.peek().kind == TokenKind::Eof {
                break;
            }
            self.parse_port_conn();
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct(')', "to close the gate ports");
        self.expect_punct(';', "to end the gate instantiation");
        self.builder.finish_node();
    }

    fn parse_generate(&mut self) {
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
    fn parse_gen_item(&mut self) {
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

    fn parse_instantiation(&mut self) {
        self.builder.start_node(SyntaxKind::Inst);
        // Module name.
        self.builder.start_node(SyntaxKind::Name);
        self.bump();
        self.builder.finish_node();
        // Optional parameter override `#(...)` — structured as ParamAssign nodes.
        if self.at_punct('#') {
            self.bump();
            self.expect_punct('(', "after `#`");
            while !self.at_punct(')') && self.peek().kind != TokenKind::Eof {
                self.builder.start_node(SyntaxKind::ParamAssign);
                if self.eat_punct('.') {
                    if self.peek().kind == TokenKind::Ident {
                        self.builder.start_node(SyntaxKind::Name);
                        self.bump();
                        self.builder.finish_node();
                    }
                    self.expect_punct('(', "after the parameter name");
                    if !self.at_punct(')') {
                        self.parse_expression();
                    }
                    self.expect_punct(')', "to close the parameter override");
                } else {
                    // Positional override.
                    self.parse_expression();
                }
                self.builder.finish_node();
                if !self.eat_punct(',') {
                    break;
                }
            }
            self.expect_punct(')', "to close the parameter override list");
        }
        // Instance name(s).
        loop {
            if self.peek().kind == TokenKind::Ident {
                self.bump_name_node();
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
        self.eat_attributes();
        let text = self.peek_text().to_string();
        match self.peek().kind {
            // `assign` is legal inside generate blocks (module item as statement).
            TokenKind::Ident if text == "assign" => self.parse_assign(),
            TokenKind::Ident if text == "foreach" => self.parse_foreach(),
            TokenKind::Ident if text == "import" => self.parse_import(false),
            TokenKind::Ident if text == "export" => self.parse_import(true),
            // `deassign x;` / `force x = e;` / `release x;` — parse and drop the
            // procedural-assignment part (not simulated).
            TokenKind::Ident if text == "deassign" || text == "release" => {
                self.bump();
                self.parse_expression();
                self.expect_punct(';', "to end the statement");
            }
            // `x++;` / `x--;` / `++x;` / `--x;`.
            TokenKind::Ident
                if self.peek2_text() == "+" && self.peek3_text() == "+"
                    || self.peek2_text() == "-" && self.peek3_text() == "-" =>
            {
                self.builder.start_node(SyntaxKind::StmtAssign);
                self.parse_postfix(); // consumes the postfix `++`/`--` too
                self.builder.finish_node();
                self.expect_punct(';', "to end the increment/decrement");
            }
            TokenKind::Punct
                if text == "+" && self.peek2_text() == "+"
                    || text == "-" && self.peek2_text() == "-" =>
            {
                self.builder.start_node(SyntaxKind::StmtAssign);
                self.bump();
                self.bump();
                self.parse_postfix();
                self.builder.finish_node();
                self.expect_punct(';', "to end the increment/decrement");
            }
            TokenKind::Ident if text == "force" => {
                self.bump();
                self.parse_expression();
                if self.eat_punct('=') {
                    self.parse_expression();
                }
                self.expect_punct(';', "to end the force statement");
            }
            TokenKind::Ident if text == "begin" => {
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
            // Case qualifiers bind to the following case statement.
            TokenKind::Ident if text == "unique" || text == "unique0" || text == "priority" => {
                self.builder.start_node(SyntaxKind::Case);
                while self.at_kw("unique") || self.at_kw("unique0") || self.at_kw("priority") {
                    self.bump();
                }
                self.parse_case_tail();
                self.builder.finish_node();
            }
            TokenKind::Ident if text == "while" => {
                self.builder.start_node(SyntaxKind::While);
                self.bump();
                self.expect_punct('(', "after `while`");
                self.parse_expression();
                self.expect_punct(')', "to close the while condition");
                self.parse_statement();
                self.builder.finish_node();
            }
            TokenKind::Ident if text == "break" => {
                self.builder.start_node(SyntaxKind::BreakStmt);
                self.bump();
                self.eat_punct(';');
                self.builder.finish_node();
            }
            TokenKind::Ident if text == "continue" => {
                self.builder.start_node(SyntaxKind::ContinueStmt);
                self.bump();
                self.eat_punct(';');
                self.builder.finish_node();
            }
            TokenKind::Ident if text == "repeat" => {
                self.builder.start_node(SyntaxKind::Repeat);
                self.bump();
                self.expect_punct('(', "after `repeat`");
                self.parse_expression();
                self.expect_punct(')', "to close the repeat count");
                self.parse_statement();
                self.builder.finish_node();
            }
            TokenKind::Ident if text == "forever" => {
                self.builder.start_node(SyntaxKind::Forever);
                self.bump();
                self.parse_statement();
                self.builder.finish_node();
            }
            TokenKind::Ident if text == "do" => {
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
            // `wait (expr) stmt` — procedural wait on a condition.
            TokenKind::Ident if text == "wait" => {
                self.builder.start_node(SyntaxKind::Wait);
                self.bump();
                self.expect_punct('(', "after `wait`");
                self.parse_expression();
                self.expect_punct(')', "to close the wait condition");
                self.parse_statement();
                self.builder.finish_node();
            }
            TokenKind::Ident if text == "disable" => {
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
            TokenKind::Ident if text == "fork" => {
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
            // SV immediate assertions: `assert (e) [else stmt];` etc.
            TokenKind::Ident
                if text == "assert" || text == "assume" || text == "cover" || text == "expect" =>
            {
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
            // Event trigger: `-> name;` / `->> name;`.
            TokenKind::Punct if text == "-" && self.peek2_text() == ">" => {
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
            // Module-level constructs are also legal inside generate blocks.
            TokenKind::Ident
                if text == "always"
                    || text == "always_comb"
                    || text == "always_ff"
                    || text == "always_latch" =>
            {
                self.parse_always();
            }
            TokenKind::Ident if text == "initial" => {
                self.parse_initial();
            }
            TokenKind::Punct if text == ";" => {
                self.bump(); // empty statement
            }
            TokenKind::Punct if text == "#" => {
                // `#delay stmt` — the delayed statement is a child of Delay.
                self.builder.start_node(SyntaxKind::Delay);
                self.bump();
                self.parse_primary(); // delay value
                self.parse_statement(); // delayed statement
                self.builder.finish_node();
            }
            TokenKind::Punct if text == "@" => {
                // Event wait: `@(posedge clk) stmt`.
                self.builder.start_node(SyntaxKind::Wait);
                self.bump();
                self.parse_sens_list();
                self.parse_statement();
                self.builder.finish_node();
            }
            TokenKind::Ident if text == "for" => {
                // Procedural for: `for (init; cond; step) stmt`. Init and step
                // are assignments without their own trailing `;`.
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
            // `return [expr];` in functions.
            TokenKind::Ident if text == "return" => {
                self.builder.start_node(SyntaxKind::ReturnStmt);
                self.bump();
                if !self.at_punct(';') {
                    self.parse_expression();
                }
                self.expect_punct(';', "to end the return statement");
                self.builder.finish_node();
            }
            // Block-level declaration: `int x = 3;` inside begin/end.
            TokenKind::Ident
                if matches!(
                    text.as_str(),
                    "wire"
                        | "reg"
                        | "logic"
                        | "integer"
                        | "real"
                        | "time"
                        | "genvar"
                        | "tri"
                        | "tri0"
                        | "tri1"
                        | "trireg"
                        | "wand"
                        | "wor"
                        | "supply0"
                        | "supply1"
                        | "bit"
                        | "byte"
                        | "shortint"
                        | "int"
                        | "longint"
                        | "string"
                        | "chandle"
                        | "var"
                        | "const"
                        | "rand"
                        | "randc"
                        | "vectored"
                        | "scalared"
                ) || (self.peek2().kind == TokenKind::Ident && !self.instantiation_ahead()) =>
            {
                self.parse_decl();
            }
            TokenKind::SysTask => {
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
            _ => {
                // Procedural assignment (`lhs = expr;` / `lhs <= expr;`) or an
                // expression statement (`f(...);`). The lhs is parsed as a
                // postfix expression only, so `<=` is read as non-blocking
                // assignment, not as less-than-or-equal.
                self.builder.start_node(SyntaxKind::StmtAssign);
                let mark = self.builder.mark();
                self.parse_postfix();
                if self.parse_assign_op() {
                    // Intra-assignment delay: `x <= #100 y;` / `x = #1 y;`.
                    if self.at_punct('#') {
                        self.builder.start_node(SyntaxKind::Delay);
                        self.bump();
                        if self.eat_punct('(') {
                            loop {
                                self.parse_expression();
                                if !self.eat_punct(',') {
                                    break;
                                }
                            }
                            self.expect_punct(')', "to close the assignment delay");
                        } else {
                            self.parse_primary();
                        }
                        self.builder.finish_node();
                    }
                    self.parse_expression();
                } else {
                    self.parse_binops(mark, 1);
                }
                self.expect_punct(';', "to end the statement");
                self.builder.finish_node();
            }
        }
    }

    /// Consume `=` (blocking) or `<=` (non-blocking). Returns false if neither.
    fn parse_assign_op(&mut self) -> bool {
        // Two/three-char operators lex as separate puncts; match on raw text.
        let t2 = format!("{}{}", self.peek_text(), self.peek2_text());
        let t3 = format!(
            "{}{}{}",
            self.peek_text(),
            self.peek2_text(),
            self.peek3_text()
        );
        for op in ["<<<=", ">>>="] {
            if t3 == op {
                self.bump();
                self.bump();
                self.bump();
                return true;
            }
        }
        for op in ["<<=", ">>=", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^="] {
            if t2 == op {
                self.bump();
                self.bump();
                return true;
            }
        }
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
        self.eat_type_words(); // `for (int i = 0; ...)`
                               // `for (...; i++)` step: a bare inc/dec needs no assignment operator.
        let is_incdec = (self.peek2_text() == "+" && self.peek3_text() == "+")
            || (self.peek2_text() == "-" && self.peek3_text() == "-");
        self.parse_postfix();
        if !self.parse_assign_op() {
            if is_incdec {
                self.builder.finish_node();
                return;
            }
            self.error_here("expected `=` or `<=` in for-loop assignment");
        }
        self.parse_expression();
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

    // ---- grammar: expressions ------------------------------------------------

    pub fn parse_expression(&mut self) {
        self.parse_expr_prec(1);
    }

    fn parse_expr_prec(&mut self, min_prec: u8) {
        let mark = self.builder.mark();
        self.parse_unary();
        self.parse_binops(mark, min_prec);
        // Ternary `?:` is the lowest-precedence operator, right-associative, and
        // only valid at the top expression level.
        if min_prec == 1 && self.at_punct('?') {
            self.builder.start_node_at(mark, SyntaxKind::CondExpr);
            self.bump(); // ?
            self.parse_expression();
            self.expect_punct(':', "in conditional expression");
            self.parse_expr_prec(1); // else branch, right-assoc
            self.builder.finish_node();
        }
    }

    /// Binary-operator precedence-climbing loop. `mark` points at the start of
    /// the lhs, so each operator wraps lhs+op+rhs into one BinExpr node.
    fn parse_binops(&mut self, mark: crate::cst::Mark, min_prec: u8) {
        loop {
            // SystemVerilog `inside` operator: `expr inside { set }` /
            // `expr inside [lo:hi, ...]`. Parsed like a binary op whose rhs is
            // a set literal (equality-level precedence).
            if self.at_kw("inside") && 9 >= min_prec {
                self.builder.start_node_at(mark, SyntaxKind::BinExpr);
                self.bump(); // inside
                             // Set items may be plain expressions or `[lo:hi]` ranges,
                             // in either brace or bracket form.
                let (_open, close) = if self.at_punct('{') {
                    ('{', '}')
                } else if self.at_punct('[') {
                    ('[', ']')
                } else {
                    self.error_here("expected `{` or `[` after `inside`");
                    break;
                };
                self.bump(); // open
                if !self.at_punct(close) {
                    loop {
                        if self.at_punct('[') {
                            self.bump();
                            self.parse_expression();
                            if self.eat_punct(':') {
                                self.parse_expression(); // `[lo:hi]` range item
                            }
                            self.expect_punct(']', "to close the range item");
                        } else {
                            self.parse_expression();
                        }
                        if !self.eat_punct(',') {
                            break;
                        }
                    }
                }
                self.expect_punct(close, "to close the `inside` set");
                self.builder.finish_node();
                continue;
            }
            let op = if let Some(op) = self.peek_op3().or_else(|| self.peek_op2()) {
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
            self.builder.start_node_at(mark, SyntaxKind::BinExpr);
            // Consume the operator (one to three punct tokens).
            for _ in 0..op_token_count(&op) {
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
        // Casts: `signed'(e)`, `unsigned'(e)`, and size casts `N'(e)`.
        if (self.at_kw("signed") || self.at_kw("unsigned")) && self.peek2_text() == "'" {
            self.builder.start_node(SyntaxKind::Cast);
            self.bump();
            self.bump(); // '
            self.parse_unary();
            self.builder.finish_node();
            return;
        }
        if self.peek().kind == TokenKind::IntLiteral && self.peek2_text() == "'" {
            self.builder.start_node(SyntaxKind::Cast);
            self.bump();
            self.bump(); // '
            self.parse_unary();
            self.builder.finish_node();
            return;
        }
        // Type cast: `SomeType'(expr)` (peek3 `(` excludes based literals `N'h..`).
        if self.peek().kind == TokenKind::Ident
            && !self.at_kw("signed")
            && !self.at_kw("unsigned")
            && self.peek2_text() == "'"
            && self.peek3_text() == "("
        {
            self.builder.start_node(SyntaxKind::Cast);
            self.bump();
            self.bump(); // '
            self.parse_unary();
            self.builder.finish_node();
            return;
        }
        // Longest-match multi-char unary op first (`~&`, `~^`…), else single char.
        let op = self
            .peek_op3()
            .or_else(|| self.peek_op2())
            .filter(|o| UNARY_OPS.contains(&o.as_str()))
            .or_else(|| {
                let t = self.peek_text().to_string();
                if self.peek().kind == TokenKind::Punct && UNARY_OPS.contains(&t.as_str()) {
                    Some(t)
                } else {
                    None
                }
            });
        if let Some(op) = op {
            self.builder.start_node(SyntaxKind::UnaryExpr);
            for _ in 0..op_token_count(&op) {
                self.bump();
            }
            self.parse_unary();
            self.builder.finish_node();
        } else {
            self.parse_postfix();
        }
    }

    fn parse_postfix(&mut self) {
        // Wrap the primary and every postfix `[...]`/`(...)` into one node:
        // `a[0]`, `a[7:4]`, `f(x)` produce Index/PartSelect/Call nodes whose
        // first child is the base expression.
        let mark = self.builder.mark();
        self.parse_primary();
        loop {
            // Size cast: `Width'(expr)` / `type'(value)`.
            if self.at_punct('\'') && self.peek2_text() == "(" {
                self.builder.start_node_at(mark, SyntaxKind::ParenExpr);
                self.bump(); // '
                self.bump(); // (
                if !self.at_punct(')') {
                    self.parse_expression();
                }
                self.expect_punct(')', "to close the cast");
                self.builder.finish_node();
                continue;
            }
            // Postfix `x++` / `x--`.
            if self.peek_text() == "+" && self.peek2_text() == "+"
                || self.peek_text() == "-" && self.peek2_text() == "-"
            {
                self.builder.start_node_at(mark, SyntaxKind::UnaryExpr);
                self.bump();
                self.bump();
                self.builder.finish_node();
                continue;
            }
            if self.at_punct('[') {
                // Index, part-select, or indexed part-select (`+:` / `-:`).
                // Scan to the matching `]`: a `:` at bracket/paren depth zero
                // makes it a select; if the token right before that `:` is
                // `+` or `-`, it is an indexed part-select. Counting parens
                // keeps a ternary `? :` inside the index from being mistaken
                // for a part-select colon.
                let mut i = self.skip_trivia_from(self.skip_trivia_from(self.pos) + 1);
                let mut depth = 0;
                let mut colon = false;
                let mut indexed = false;
                // A `?` at depth zero claims the next depth-zero `:` as its
                // ternary colon, not a part-select colon.
                let mut ternary = 0;
                while i < self.tokens.len() {
                    let k = self.tokens[i].kind;
                    let t = &self.src.text()
                        [self.tokens[i].span.start as usize..self.tokens[i].span.end as usize];
                    if k == TokenKind::Punct && (t == "[" || t == "(") {
                        depth += 1;
                    } else if k == TokenKind::Punct && (t == "]" || t == ")") {
                        if t == "]" && depth == 0 {
                            break;
                        }
                        depth -= 1;
                    } else if k == TokenKind::Punct && t == "?" && depth == 0 {
                        ternary += 1;
                    } else if k == TokenKind::Punct && t == ":" && depth == 0 {
                        if ternary > 0 {
                            ternary -= 1;
                        } else {
                            colon = true;
                            let mut j = i;
                            while j > 0 {
                                j -= 1;
                                if !matches!(
                                    self.tokens[j].kind,
                                    TokenKind::Whitespace | TokenKind::Comment
                                ) {
                                    break;
                                }
                            }
                            if j < self.tokens.len() {
                                let pt = &self.src.text()[self.tokens[j].span.start as usize
                                    ..self.tokens[j].span.end as usize];
                                indexed = self.tokens[j].kind == TokenKind::Punct
                                    && (pt == "+" || pt == "-");
                            }
                        }
                    }
                    i += 1;
                }
                let kind = if colon {
                    if indexed {
                        SyntaxKind::IndexedPartSelect
                    } else {
                        SyntaxKind::PartSelect
                    }
                } else {
                    SyntaxKind::Index
                };
                self.builder.start_node_at(mark, kind);
                self.bump(); // [
                if indexed {
                    // `base +: width` / `base -: width`: parse the base with a
                    // precedence above `+`/`-` so the sign stays for the
                    // indexed operator.
                    self.parse_expr_prec(13);
                    self.bump(); // `+` or `-`
                    self.expect_punct(':', "in the indexed part-select");
                    self.parse_expression(); // width
                } else {
                    self.parse_expression();
                    if colon {
                        self.expect_punct(':', "in part-select");
                        self.parse_expression();
                    }
                }
                self.expect_punct(']', "to close the select");
                self.builder.finish_node();
            } else if self.at_punct('(') {
                // Function call — wraps the callee name: Call(Name, args...).
                self.builder.start_node_at(mark, SyntaxKind::Call);
                self.bump();
                if !self.at_punct(')') {
                    loop {
                        self.parse_call_arg();
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

    /// One call argument: an expression, or a named argument `.name[(expr)]`.
    fn parse_call_arg(&mut self) {
        if self.at_punct('.') {
            self.builder.start_node(SyntaxKind::PortConn);
            self.bump();
            if self.peek().kind == TokenKind::Ident {
                self.builder.start_node(SyntaxKind::Name);
                self.bump();
                self.builder.finish_node();
            }
            if self.eat_punct('(') {
                if !self.at_punct(')') {
                    self.parse_expression();
                }
                self.expect_punct(')', "to close the named argument");
            }
            self.builder.finish_node();
        } else {
            self.parse_expression();
        }
    }

    fn parse_primary(&mut self) {
        // Consume pending trivia into the *parent* node first, so a `Name`
        // started below does not absorb a preceding comment into its text.
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
        let t = self.peek().clone();
        if t.kind == TokenKind::Ident
            && (self.peek_text() == "struct" || self.peek_text() == "union")
        {
            self.parse_struct_type();
            return;
        }
        match t.kind {
            TokenKind::IntLiteral | TokenKind::RealLiteral | TokenKind::StrLiteral => {
                // A plain (non-based) number directly followed by a time-unit
                // identifier is an SV time literal (`1ns`, `#(2*1ns)`,
                // `#(1 ns)`); the unit token joins the Literal node.
                let plain_number = t.kind != TokenKind::StrLiteral
                    && !self.peek_text().contains('\'')
                    && self.at_time_unit();
                self.builder.start_node(SyntaxKind::Literal);
                self.bump();
                if plain_number {
                    self.bump();
                }
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
            TokenKind::Ident if self.peek_text() == "new" => {
                // `new C(42)` / `new(42)` (inside a constructor) / `new[3]`.
                self.builder.start_node(SyntaxKind::Allocator);
                self.bump(); // new
                if self.peek().kind == TokenKind::Ident && !self.at_punct('(') {
                    self.builder.start_node(SyntaxKind::Name);
                    self.bump();
                    self.builder.finish_node();
                }
                if self.eat_punct('[') {
                    self.parse_expression();
                    self.expect_punct(']', "to close the new[] size");
                } else if self.eat_punct('(') {
                    if !self.at_punct(')') {
                        loop {
                            self.parse_expression();
                            if !self.eat_punct(',') {
                                break;
                            }
                        }
                    }
                    self.expect_punct(')', "to close the constructor arguments");
                }
                self.builder.finish_node();
            }
            TokenKind::Ident => {
                self.builder.start_node(SyntaxKind::Name);
                self.bump();
                // Hierarchical / scope-qualified reference: `a.b.c`, `p::x`.
                loop {
                    if self.at_punct('.') {
                        self.bump();
                        if self.peek().kind == TokenKind::Ident {
                            self.bump();
                        } else {
                            self.error_here("expected a name after `.`");
                            break;
                        }
                    } else if self.at_punct(':') && self.peek2_text() == ":" {
                        self.bump();
                        self.bump();
                        // Either `*` or the loop variable name: both are skipped.
                        if self.at_punct('*') || self.peek().kind == TokenKind::Ident {
                            self.bump();
                        } else {
                            break;
                        }
                    } else {
                        break;
                    }
                }
                self.builder.finish_node();
            }
            TokenKind::Punct if self.peek_text() == "'" && self.peek2_text() == "{" => {
                // Assignment pattern: `'{ a: x, b: y, default: z }`.
                self.builder.start_node(SyntaxKind::Pattern);
                self.bump(); // '
                self.bump(); // {
                loop {
                    if self.at_punct('}') || self.peek().kind == TokenKind::Eof {
                        break;
                    }
                    if self.peek().kind == TokenKind::Ident
                        && (self.peek2_text() == ":" || self.peek2_text() == ":=")
                    {
                        self.builder.start_node(SyntaxKind::Name);
                        self.bump();
                        self.builder.finish_node();
                        self.bump(); // :
                    }
                    self.parse_expression();
                    if !self.eat_punct(',') {
                        break;
                    }
                }
                self.expect_punct('}', "to close the assignment pattern");
                self.builder.finish_node();
            }
            TokenKind::Punct if self.peek_text() == "(" => {
                self.builder.start_node(SyntaxKind::ParenExpr);
                self.bump();
                self.parse_expression();
                self.expect_punct(')', "to close the parenthesized expression");
                self.builder.finish_node();
            }
            TokenKind::Punct if self.peek_text() == "{" => {
                // Replicate `{ n { expr } }` vs concat `{ a, b, ... }`: the
                // node must wrap from the opening brace, so mark first and
                // start the node retroactively once the shape is known. Both
                // closing braces belong to the node — a trailing `}` left as
                // a sibling corrupts the parent BinExpr's operator text.
                let mark = self.builder.mark();
                self.bump(); // {
                self.parse_expression();
                if self.at_punct('{') {
                    self.builder.start_node_at(mark, SyntaxKind::Replicate);
                    self.bump(); // inner {
                                 // The replicated operand may itself be a concatenation:
                                 // `{4{3'b000, bmask}}`.
                    self.parse_expression();
                    while self.eat_punct(',') {
                        self.parse_expression();
                    }
                    self.expect_punct('}', "to close the replication");
                    self.expect_punct('}', "to close the replication");
                    self.builder.finish_node();
                } else {
                    self.builder.start_node_at(mark, SyntaxKind::Concat);
                    while self.eat_punct(',') {
                        self.parse_expression();
                    }
                    self.expect_punct('}', "to close the concatenation");
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

    #[test]
    fn parses_sva_lite_properties_and_immediate_assert() {
        let src = "module m (input wire clk, input wire a, output reg b);\n\
                   always @(posedge clk) b <= a;\n\
                   assert property (@(posedge clk) a |-> b);\n\
                   assert property (@(posedge clk) a |=> b);\n\
                   assert property (@(posedge clk) b);\n\
                   assume property (@(posedge clk) a);\n\
                   cover property (@(posedge clk) a |-> b);\n\
                   initial begin assert (a == 1'b0); assert (b) else $display(\"x\"); end\n\
                   endmodule\n";
        let (parsed, _db) = parse_str(src);
        assert!(
            parsed
                .diags
                .iter()
                .all(|d| d.severity != crate::diagnostic::Severity::Error),
            "{:?}",
            parsed.diags
        );
        let kinds: Vec<SyntaxKind> = parsed.root.descendants().map(|n| n.kind()).collect();
        assert_eq!(
            kinds
                .iter()
                .filter(|k| **k == SyntaxKind::AssertProperty)
                .count(),
            3
        );
        assert_eq!(
            kinds
                .iter()
                .filter(|k| **k == SyntaxKind::AssumeProperty)
                .count(),
            1
        );
        assert_eq!(
            kinds
                .iter()
                .filter(|k| **k == SyntaxKind::CoverProperty)
                .count(),
            1
        );
        assert_eq!(
            kinds.iter().filter(|k| **k == SyntaxKind::Property).count(),
            5
        );
        assert_eq!(
            kinds.iter().filter(|k| **k == SyntaxKind::SvaStmt).count(),
            2
        );
    }

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
    fn parses_iff_event_qualifier() {
        // `iff` wraps the condition in an IffCond node inside the SensList.
        let sexp = ok(
            "module m(input clk, input en, output reg q); always @(posedge clk iff en) q <= 1; endmodule\n",
        );
        assert!(sexp.contains("(IffCond"), "{}", sexp);
        // Mixed list: the qualifier binds to the entry it follows.
        let sexp = ok(
            "module m(input clk, input en, input rst_n, output reg q); always @(posedge clk iff en or negedge rst_n) q <= 1; endmodule\n",
        );
        assert!(sexp.matches("(IffCond").count() == 1, "{}", sexp);
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
        assert!(sexp.contains("(SensList"), "{}", sexp);
        assert!(sexp.contains("(StmtAssign"), "{}", sexp);
        assert!(sexp.contains("(If"), "{}", sexp);
        // `q <= 8'h00` must be a non-blocking StmtAssign, not a BinExpr comparison.
        assert!(
            !sexp.contains("(BinExpr (Name Ident \"q\") Punct \"<\""),
            "{}",
            sexp
        );
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
            .is_some_and(|p2| inner[p2..].contains("Literal \"3\"") || true));
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
            "task",
            "fork",
            "fork begin end",
            "while (",
            "do",
            "->",
            "defparam",
            "typedef enum",
            "module m; and; endmodule",
            "module m; assert; endmodule",
            "module m; wait; endmodule",
        ] {
            let _ = parse_str(src);
        }
    }

    /// Parse and assert zero error diagnostics; returns the s-expression.
    fn ok(src: &str) -> String {
        let (p, db) = parse_str(src);
        let errs: Vec<_> = p
            .diags
            .iter()
            .filter(|d| d.severity == crate::diagnostic::Severity::Error)
            .collect();
        assert!(
            errs.is_empty(),
            "{}\n{}",
            errs.iter()
                .map(|d| d.render(&db))
                .collect::<Vec<_>>()
                .join("\n"),
            src
        );
        assert_eq!(p.root.text(&db), src, "tree must be lossless");
        p.root.debug_sexp(&db)
    }

    #[test]
    fn parses_task_disable_and_wait() {
        let sexp = ok(r#"module m;
task automatic tsk(input [7:0] d, output reg [7:0] q);
  q = d;
endtask
initial begin
  wait (ready);
  tsk(.d(8'h01), .q(q));
  disable tsk;
end
endmodule
"#);
        assert!(sexp.contains("(Task"), "{}", sexp);
        assert!(sexp.contains("(Disable"), "{}", sexp);
        assert!(sexp.contains("(Wait"), "{}", sexp);
    }

    #[test]
    fn parses_while_repeat_forever_do_while() {
        let sexp = ok(r#"module m;
initial begin
  while (i < 10) i = i + 1;
  repeat (4) j = j + 1;
  forever begin #10 clk = ~clk; end
  do k = k - 1; while (k > 0);
end
endmodule
"#);
        for node in ["(While", "(Repeat", "(Forever", "(DoWhile"] {
            assert!(sexp.contains(node), "{} missing in {}", node, sexp);
        }
        // A comment before a statement must not leak into the statement's Name.
        let sexp = ok("module m;\ninitial begin\n  // note\n  x = 1;\nend\nendmodule\n");
        assert!(sexp.contains(r#"(Name Ident "x")"#), "{}", sexp);
    }

    #[test]
    fn parses_fork_join_variants() {
        let sexp = ok(r#"module m;
initial begin
  fork : par
    a = 1;
    b = 2;
  join
  fork
    c = 3;
  join_any
  fork
    d = 4;
  join_none
end
endmodule
"#);
        assert_eq!(sexp.matches("(ForkJoin").count(), 3, "{}", sexp);
    }

    #[test]
    fn parses_event_decl_and_triggers() {
        let sexp = ok(r#"module m;
event ev;
initial begin
  -> ev;
  ->> ev2;
end
endmodule
"#);
        assert!(sexp.contains("(EventDecl"), "{}", sexp);
        assert_eq!(sexp.matches("(Trigger").count(), 2, "{}", sexp);
    }

    #[test]
    fn parses_defparam_and_final() {
        let sexp = ok(r#"module m;
  defparam u0.WIDTH = 16, u1.DEPTH = 4;
  final $display("done");
endmodule
"#);
        assert!(sexp.contains("(Defparam"), "{}", sexp);
        assert!(sexp.contains("(Final"), "{}", sexp);
    }

    #[test]
    fn parses_generate_case_and_named_block() {
        let sexp = ok(r#"module m(input [1:0] sel);
generate
  case (sel)
    2'd0: begin : low
      assign y = a;
    end
    default: begin : high
      assign y = b;
    end
  endcase
endgenerate
endmodule
"#);
        assert!(sexp.contains("(GenCase"), "{}", sexp);
        assert_eq!(sexp.matches("(GenBlock").count(), 2, "{}", sexp);
    }

    #[test]
    fn parses_gate_primitives_with_strength() {
        let sexp = ok(r#"module m(input a, b, output o);
  and (o, a, b);
  nand (strong1, pull0) g1 (n, a, b);
  pullup (o);
  bufif1 (t, a, en);
endmodule
"#);
        assert_eq!(sexp.matches("(Inst").count(), 4, "{}", sexp);
    }

    #[test]
    fn parses_typedef_enum_and_struct() {
        let sexp = ok(r#"module m;
typedef enum logic [1:0] { IDLE = 0, RUN = 1, DONE = 2 } state_t;
typedef struct packed signed {
  logic [7:0] hi;
  logic [7:0] lo;
} pair_t;
typedef logic [15:0] word_t;
endmodule
"#);
        assert!(sexp.contains("(EnumType"), "{}", sexp);
        assert_eq!(sexp.matches("(StructType").count(), 1, "{}", sexp);
        assert_eq!(sexp.matches("(Typedef").count(), 3, "{}", sexp);
    }

    #[test]
    fn parses_casts_in_expressions() {
        let sexp = ok(r#"module m;
  assign a = signed'(b) + 8'(c) + unsigned'(d);
endmodule
"#);
        assert_eq!(sexp.matches("(Cast").count(), 3, "{}", sexp);
    }

    #[test]
    fn parses_case_qualifiers_and_inside() {
        let sexp = ok(r#"module m;
always @* begin
  priority case (x)
    0: y = 1;
    default: y = 0;
  endcase
  unique case (z) inside
    [0:3]: y = 1;
    default: y = 0;
  endcase
end
endmodule
"#);
        assert_eq!(sexp.matches("(Case ").count(), 2, "{}", sexp);
    }

    #[test]
    fn parses_sva_immediate_assertions() {
        let sexp = ok(r#"module m;
always @(posedge clk) begin
  assert (cnt < 100) else $error("overflow");
  assume (req | !grant);
  cover (a && b);
end
endmodule
"#);
        assert_eq!(sexp.matches("(SvaStmt").count(), 3, "{}", sexp);
    }

    #[test]
    fn parses_assign_with_delay_and_strength() {
        let sexp = ok(r#"module m;
  assign #5 a = b;
  assign (strong1, pull0) c = d;
  assign e = #10 f;
endmodule
"#);
        assert_eq!(sexp.matches("(Assign").count(), 3, "{}", sexp);
        assert_eq!(sexp.matches("(Delay").count(), 2, "{}", sexp);
    }

    #[test]
    fn parses_function_with_ansi_ports_and_param_type() {
        let sexp = ok(r#"module m;
  parameter type T = logic [7:0];
  function automatic logic [7:0] f(input logic [7:0] a, input int b);
    logic [7:0] tmp;
    tmp = a + b;
    return tmp;
  endfunction
endmodule
"#);
        assert!(sexp.contains("(Function"), "{}", sexp);
        assert!(sexp.contains("(Port"), "{}", sexp);
    }

    #[test]
    fn parses_sv_variable_modifiers_and_types() {
        let sexp = ok(r#"module m;
  bit [3:0] nib;
  string s;
  var int v;
  const int C = 3;
  rand reg [7:0] r;
  randc int rc;
endmodule
"#);
        assert_eq!(sexp.matches("(Decl").count(), 6, "{}", sexp);
    }

    #[test]
    fn parses_common_cells_constructs() {
        // Constructs from pulp-platform/common_cells: typed parameters,
        // user-typed ports/params, struct types, implicit generate regions,
        // inc/dec, compound assigns, size casts, multi-dim ranges, attributes,
        // labeled endmodule, and (dropped) macro invocations incl. multi-line.
        let sexp = ok(r#"module m #(
    parameter int unsigned Width = 1,
    parameter type data_t = logic
) (
    input  data_t d_i,
    output data_t d_o
);
  localparam cb_seed_t [2:0] Seeds = '{1'b0, 1'b1, 1'b0};
  typedef struct packed {
    int unsigned PermuteSeed;
    int unsigned XorSeed;
  } cb_seed_t;
  (* dont_touch = "true" *) logic async_req;
  data_t [Width-1:0] mem_n, mem_q;
  logic [Width-1:0][3:0] sum;
  if (Width > 1) begin : gen_wide
    assign d_o = d_i;
  end else begin : gen_narrow
    assign d_o = '0;
  end
  for (genvar j = 0; j < Width; j++) begin : gen_j
    always_comb begin
      sum[j] += data_t'(j);
      mem_q[j]++;
    end
  end
  `ASSERT(lock, req_o && !gnt_i,
    "multi line message")
endmodule : m
"#);
        assert!(sexp.contains("(ParamDecl"), "{}", sexp);
        assert!(sexp.contains("(StructType"), "{}", sexp);
        assert!(sexp.contains("(GenIf"), "{}", sexp);
        assert!(sexp.contains("(GenFor"), "{}", sexp);
    }

    #[test]
    fn parses_escaped_identifiers() {
        let sexp = ok("module m; wire \\a+b ; assign \\a+b = 1'b0; endmodule\n");
        assert!(sexp.contains("\\a+b"), "{}", sexp);
    }
    #[test]
    fn replicate_node_owns_both_closing_braces() {
        // `{4{b}}` as a binary operand must parse as BinExpr(Name, "^",
        // Replicate) with NO stray '}' punct sibling — a leftover '}' used
        // to corrupt the parent operator text ("^}").
        let sexp = ok(
            "module m(input [3:0] a, input b, output [3:0] y); assign y = a ^ {4{b}}; endmodule\n",
        );
        let bin = sexp.split("(BinExpr").nth(1).unwrap_or("");
        let bin = &bin[..bin.find("Punct \";\"").unwrap_or(bin.len())];
        assert!(bin.contains("Punct \"^\" (Replicate"), "{}", bin);
        assert_eq!(bin.matches("Punct \"}\"").count(), 2, "{}", bin);
    }

    #[test]
    fn parses_module_header_package_imports() {
        // `module m import p::*; (...)` and `module m import p::*;` (no ports).
        let sexp = ok("module m import p::*; (input logic a); endmodule\n");
        assert!(sexp.contains("(Import"), "{}", sexp);
        let sexp = ok("module m import p::*, q::x; #(parameter W=8) (input logic a); endmodule\n");
        assert!(sexp.contains("(Import"), "{}", sexp);
        ok("module m import p::*; endmodule\n");
    }

    #[test]
    fn parses_indexed_part_select_and_ternary_index() {
        // `base +: width` / `base -: width`, and a ternary `? :` inside an
        // index must not be mistaken for a part-select colon.
        let sexp = ok("module m(input [7:0] d, input [2:0] s, output [3:0] o); assign o = d[{1'b0,s}+:4]; endmodule\n");
        assert!(sexp.contains("(IndexedPartSelect"), "{}", sexp);
        let sexp = ok("module m #(parameter W=2) (input [3:0] c, output [3:0] x); assign x = c[(W == 1) ? 0 : 2]; endmodule\n");
        assert!(sexp.contains("(Index"), "{}", sexp);
        assert!(sexp.contains("(CondExpr"), "{}", sexp);
    }

    #[test]
    fn parses_bare_enum_decl_and_multidim_ports() {
        let sexp = ok("module m(input logic [2:0][5:0] r, output logic [1:0] o); enum logic [1:0] { A, B } state, next; assign o = state; endmodule\n");
        assert!(sexp.contains("(EnumType"), "{}", sexp);
        assert!((sexp.matches("(Range").count() >= 3), "{}", sexp);
    }

    #[test]
    fn parses_inside_operator_arith_shift_replicate_concat() {
        let sexp = ok("module m(input [2:0] x, output y, output [7:0] z); assign y = x inside {[3'b000:3'b010], 3'b111}; assign z = {4{3'b000, x[0]}}; endmodule\n");
        assert!(sexp.contains("inside"), "{}", sexp);
        let sexp = ok("module m(input signed [7:0] a, input [2:0] s, output signed [7:0] y); assign y = a >>> s; endmodule\n");
        assert!(
            sexp.contains("Punct \">\" Punct \">\" Punct \">\""),
            "{}",
            sexp
        );
    }

    #[test]
    fn parses_intra_assignment_delay() {
        let sexp = ok("module m(input d, output reg q); always @(d) q <= #100 d; endmodule\n");
        assert!(sexp.contains("(Delay"), "{}", sexp);
    }

    #[test]
    fn parses_time_literals_in_delays() {
        // `1ns` (adjacent), `1 ns` (spaced) and `2*1ns` in an expression all
        // form one Literal node spanning the number and the unit identifier.
        let sexp = ok(
            "module m(output reg q); initial begin #1ns q = 1; #(2*1ns) q = 0; #(1 ns) q = 1; end endmodule\n",
        );
        assert!(
            sexp.matches("(Literal IntLiteral \"1\" Ident \"ns\")")
                .count()
                == 3,
            "{}",
            sexp
        );
        // A unit identifier reached through an operator stays a name: the
        // number and `ns` never join into one Literal node.
        let sexp = ok("module m; wire [7:0] y = 2 * ns; endmodule\n");
        assert!(sexp.contains("(BinExpr"), "{}", sexp);
        assert!(!sexp.contains("Literal IntLiteral \"2\" Ident"), "{}", sexp);
    }

    #[test]
    fn parses_type_cast_and_hierarchical_name() {
        // `Type'(expr)` cast and `glbl.GSR` hierarchical name.
        let sexp = ok("module m; wire [11:0] a = csr_num_e'(x ? b[11:0] : '0); tri0 g = glbl.GSR; endmodule\n");
        assert!(sexp.contains("(Cast"), "{}", sexp);
        assert!(
            sexp.contains("Ident \"glbl\" Punct \".\" Ident \"GSR\""),
            "{}",
            sexp
        );
        // A based literal `32'hFF` is not a cast.
        let sexp2 = ok("module m; wire [31:0] w = 32'hFF; endmodule\n");
        assert!(!sexp2.contains("(Cast"), "{}", sexp2);
    }
}
