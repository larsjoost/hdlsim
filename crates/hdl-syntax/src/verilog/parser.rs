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

mod class;
mod decls;
mod expr;
mod gen;
mod inst;
mod proc;
mod stmt;
mod stream;
mod sva;
mod types;

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
) -> (ParsedFile, HashMap<String, super::preproc::Macro>) {
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

/// SV time-literal unit suffixes (`1ns`, `#(2*1us)`, SV LRM 5.7.1).
const TIME_UNITS: &[&str] = &["fs", "ps", "ns", "us", "ms", "s"];

/// Multi-char operators the lexer split into single Punct tokens.
const MULTI_OPS: &[&str] = &[
    "<<", ">>", "<<<", ">>>", "<=", ">=", "==", "!=", "===", "!==", "&&", "||", "**", "~&", "~|",
    "~^", "^~",
];

/// Keywords that open a data (net/reg/variable) declaration in a module
/// body, a function/task body, or a statement position. One table for all
/// three dispatch sites, so the keyword sets cannot drift apart.
pub(super) const DECL_KEYWORDS: &[&str] = &[
    "wire", "reg", "logic", "integer", "real", "time", "genvar", "tri", "tri0", "tri1", "trireg",
    "wand", "wor", "supply0", "supply1", "bit", "byte", "shortint", "int", "longint", "string",
    "chandle", "vectored", "scalared", "var", "const", "rand", "randc",
];

/// Gate primitive keywords (`and (o, a, b);` et al., IEEE 1364-2005 ch. 7).
const GATE_KEYWORDS: &[&str] = &[
    "and", "nand", "or", "nor", "xor", "xnor", "buf", "not", "bufif0", "bufif1", "notif0",
    "notif1", "tran", "tranif0", "tranif1", "rtran", "rtranif0", "rtranif1", "pullup", "pulldown",
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

    /// After `endclass`/`endpackage`: the optional trailing name — only if
    /// it matches the construct's name AND is followed by `;`, so a
    /// following declaration (`endclass\nC c;`) is not eaten.
    fn eat_matching_trailing_name(&mut self, name: &str) {
        if !name.is_empty()
            && self.peek().kind == TokenKind::Ident
            && self.peek_text() == name
            && self.peek2().kind == TokenKind::Punct
            && self.peek2_text() == ";"
        {
            self.bump();
        }
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
        self.eat_matching_trailing_name(&pname);
        self.eat_punct(';'); // optional
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

    /// One module-body item (after attributes): dispatch on the leading
    /// token. Macro invocations and directives are skipped; SystemVerilog
    /// `assert`-family items are SVA-lite properties.
    fn parse_module_item(&mut self) {
        self.eat_attributes();
        let t = self.peek().clone();
        let text = self.peek_text().to_string();
        match t.kind {
            TokenKind::Ident => self.parse_module_item_ident(&text),
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

    /// Dispatch on the leading keyword of a module-body item. The keyword
    /// sets are disjoint, so table membership and literal arms are
    /// interchangeable.
    fn parse_module_item_ident(&mut self, text: &str) {
        match text {
            "input" | "output" | "inout" => self.parse_port_decl_stmt(),
            _ if DECL_KEYWORDS.contains(&text) => self.parse_decl(),
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
            "assert" | "assume" | "cover" => self.parse_sva_property(text),
            // `endmodule` here means the enclosing construct was never
            // closed — report and skip so no loop can spin on it.
            "endmodule" => {
                self.error_here("unexpected `endmodule`");
                self.bump();
            }
            _ if GATE_KEYWORDS.contains(&text) => self.parse_gate_inst(),
            "specify" => self.parse_specify_block(),
            _ => self.parse_module_item_other(text),
        }
    }

    /// A leading identifier that is no known keyword: a module
    /// instantiation (`modname [#(...)] inst (...)`), a user-typed
    /// declaration, or a reported error.
    fn parse_module_item_other(&mut self, text: &str) {
        // A name list followed by `(` or `#` is a module instantiation;
        // anything else (`type_t a, b;`) is a declaration with a (possibly
        // package-imported) type.
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

    /// `specify ... endspecify` timing blocks: parsed as a skipped node.
    fn parse_specify_block(&mut self) {
        self.builder.start_node(SyntaxKind::Specify);
        self.bump();
        while !self.at_kw("endspecify") && self.peek().kind != TokenKind::Eof {
            self.bump();
        }
        self.eat_kw("endspecify");
        self.builder.finish_node();
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
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod tests_stmt;
