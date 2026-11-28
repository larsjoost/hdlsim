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
use crate::diagnostic::Diagnostic;
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

mod decl;
mod design;
mod iface;
mod lex;
mod types;

struct Parser<'a> {
    src: &'a SourceFile,
    tokens: &'a [Token],
    pos: usize,
    builder: TreeBuilder,
    diags: Vec<Diagnostic>,
    /// >0 while parsing a waveform value, where `when`/`else` belongs to the
    /// > waveform (conditional signal assignment), not to a conditional
    /// > expression.
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

    /// Sequential block statement:
    /// `[label:] block [(guard)] [is] <decls> begin <seq stmts> end block [label];`
    fn parse_block_stmt(&mut self) {
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
        if self
            .tokens
            .get(i)
            .is_some_and(|t| matches!(t.kind, TokenKind::Ident | TokenKind::StrLiteral))
        {
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
                        .is_some_and(|t| t.kind == TokenKind::Ident)
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
        if self.eat_kw("severity") && self.peek().kind == TokenKind::Ident {
            self.bump();
        }
        self.expect_punct(';', "to end the assert statement");
        self.builder.finish_node();
    }

    fn parse_seq_stmt(&mut self) {
        if self.at_label() {
            self.bump(); // label
            self.bump(); // ':'
        }
        if self.at_kw("if") {
            self.parse_if();
        } else if self.at_kw("case") {
            self.parse_case();
        } else if self.at_kw("block") {
            // Sequential block statement: `lbl : block ... begin ... end block;`
            self.parse_block_stmt();
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
        self.eat_punct('?'); // `end case?;`
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

    /// A name: `ident` with selected (`.b`), indexed (`(i)`), sliced
    /// (`(msb downto lsb)`), called (`(args)`), or attributed (`'attr`) suffixes.
    /// VHDL-2008 external name: `<< signal|constant|variable path : subtype >>`.
    fn parse_ext_name(&mut self) {
        self.start(SyntaxKind::ExtName);
        self.bump();
        self.bump(); // <<
        if self.peek().kind == TokenKind::Ident {
            self.bump(); // signal | constant | variable
        }
        self.parse_external_path();
        self.expect_punct(':', "in the external name");
        self.parse_type_mark();
        if self.at_punct('(') {
            self.parse_constraint();
        }
        if self.at_punct2(">>") {
            self.bump();
            self.bump(); // >>
        } else {
            self.error_here("expected '>>' to close the external name");
        }
        self.builder.finish_node();
    }

    /// External-name path: `[^] [. name]...` with optional indices and a
    /// trailing `.all` (e.g. `^.Transmitter_1.TransRec`, `dut.bus.all`).
    fn parse_external_path(&mut self) {
        self.start(SyntaxKind::Name);
        if self.at_punct('^') {
            self.bump();
        }
        if self.at_punct('.') {
            self.bump();
        }
        loop {
            if matches!(
                self.peek().kind,
                TokenKind::Ident | TokenKind::StrLiteral | TokenKind::CharLiteral
            ) {
                self.bump();
            } else {
                self.error_here("expected a name in the external path");
                break;
            }
            if self.at_punct('(') {
                let mut depth = 1;
                self.bump();
                while depth > 0 && self.peek().kind != TokenKind::Eof {
                    if self.at_punct('(') {
                        depth += 1;
                    } else if self.at_punct(')') {
                        depth -= 1;
                    }
                    self.bump();
                }
            }
            if self.at_punct('.') {
                self.bump();
                if self.at_kw("all") {
                    self.bump();
                    break; // trailing `.all`
                }
                continue;
            }
            break;
        }
        self.builder.finish_node();
    }

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
            // VHDL-2019 generic map on a subprogram call:
            // `f generic map (T => u) (args);` — skip the map, then the args.
            if self.at_kw("generic") && self.peek2_text().eq_ignore_ascii_case("map") {
                self.bump();
                self.bump();
                self.parse_assoc_list();
                continue;
            }
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
        // VHDL-2008 force/release drive control: `s <= force '1';` /
        // `s <= release '0';` (also per waveform element). For parse/lower
        // purposes the value itself is the waveform element.
        if self.at_kw("force") || self.at_kw("release") {
            self.bump();
        }
        // VHDL-2008 `unaffected` waveform element: schedules no transaction.
        // Bump the keyword as a bare child of the assignment; the lowerer
        // sees no value expression for the element and skips it.
        if self.at_kw("unaffected") {
            self.bump();
            self.in_waveform -= 1;
            return;
        }
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
        // VHDL-2019 allows omitting the final `else` (`v when c`) — the lowerer
        // treats the missing else branch as X.
        // Not in waveform position (there `when` starts a conditional waveform).
        if self.in_waveform == 0 && self.at_kw("when") {
            self.builder.start_node_at(mark, SyntaxKind::CondExpr);
            self.bump(); // when
            self.parse_expr_prec(1); // condition
            if self.at_kw("else") {
                self.bump();
                self.parse_expr_prec(1); // else branch (chains further when/else)
            }
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
        while let Some(op) = self.current_binop() {
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
                // Sized bit-string literal: `32X"0"`, `32SB"1"`, `32UB"1"` —
                // the size integer is directly followed by an optional `s`
                // (signed) or `u` (unsigned, VHDL-2019), the base letter, and
                // the string.
                if self.peek().kind == TokenKind::Ident
                    && (self.peek_text().len() == 1
                        || (self.peek_text().len() == 2
                            && matches!(self.peek_text().as_bytes()[0], b's' | b'S' | b'u' | b'U')))
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
            TokenKind::Punct if self.at_punct2("<<") => {
                // VHDL-2008 external name: `<< signal dut.s : std_logic >>`
                self.parse_ext_name();
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
mod tests;

#[cfg(test)]
mod tests_stmt;
