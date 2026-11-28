//! Verilog expression grammar: precedence climbing over the full IEEE
//! 1364-2005 operator set, unary/cast prefixes, postfix selects and calls,
//! and the primaries (literals, names, concatenation/replication, streaming,
//! assignment patterns, `new`, system calls). Child module of
//! `verilog::parser` (house pattern: cohesive grammar slices live in child
//! modules to keep the parser file at its standards baseline).

use crate::cst::Mark;
use crate::kinds::SyntaxKind;
use crate::token::TokenKind;

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

/// Number of punct tokens in a multi-char operator string.
fn op_token_count(op: &str) -> usize {
    op.chars().count()
}

impl super::Parser<'_> {
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
    pub(super) fn parse_binops(&mut self, mark: Mark, min_prec: u8) {
        loop {
            // SystemVerilog `inside` operator: `expr inside { set }` /
            // `expr inside [lo:hi, ...]`. Parsed like a binary op whose rhs is
            // a set literal (equality-level precedence).
            if self.at_kw("inside") && 9 >= min_prec {
                self.builder.start_node_at(mark, SyntaxKind::BinExpr);
                self.bump(); // inside
                self.parse_inside_set();
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

    /// The set after an `inside` operator: items are plain expressions or
    /// `[lo:hi]` ranges, in either brace or bracket form.
    fn parse_inside_set(&mut self) {
        let (_open, close) = if self.at_punct('{') {
            ('{', '}')
        } else if self.at_punct('[') {
            ('[', ']')
        } else {
            self.error_here("expected `{` or `[` after `inside`");
            return;
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
    }

    pub(super) fn parse_unary(&mut self) {
        if self.at_cast_start() {
            self.parse_cast();
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

    /// A cast prefix: `signed'(e)`, `unsigned'(e)`, a size cast `N'(e)`, or a
    /// type cast `SomeType'(expr)` (peek3 `(` excludes based literals `N'h..`).
    fn at_cast_start(&self) -> bool {
        if (self.at_kw("signed") || self.at_kw("unsigned")) && self.peek2_text() == "'" {
            return true;
        }
        if self.peek().kind == TokenKind::IntLiteral && self.peek2_text() == "'" {
            return true;
        }
        self.peek().kind == TokenKind::Ident
            && !self.at_kw("signed")
            && !self.at_kw("unsigned")
            && self.peek2_text() == "'"
            && self.peek3_text() == "("
    }

    /// `type'(expr)` with the Cast node opened here.
    fn parse_cast(&mut self) {
        self.builder.start_node(SyntaxKind::Cast);
        self.bump();
        self.bump(); // '
        self.parse_unary();
        self.builder.finish_node();
    }

    pub(super) fn parse_postfix(&mut self) {
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
                self.parse_select(mark);
            } else if self.at_punct('(') {
                self.parse_call_tail(mark);
            } else {
                break;
            }
        }
    }

    /// The node kind for a `[...]` postfix, from its token shape: `a[i]` is
    /// an Index, `a[msb:lsb]` a PartSelect, `a[base +: width]` an
    /// IndexedPartSelect.
    fn select_kind(&self) -> SyntaxKind {
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
                    indexed = self.token_before_is_add_sub(i);
                }
            }
            i += 1;
        }
        if colon {
            if indexed {
                SyntaxKind::IndexedPartSelect
            } else {
                SyntaxKind::PartSelect
            }
        } else {
            SyntaxKind::Index
        }
    }

    /// Is the last non-trivia token before index `i` a `+` or `-` punct?
    fn token_before_is_add_sub(&self, i: usize) -> bool {
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
        if j >= self.tokens.len() {
            return false;
        }
        let pt =
            &self.src.text()[self.tokens[j].span.start as usize..self.tokens[j].span.end as usize];
        self.tokens[j].kind == TokenKind::Punct && (pt == "+" || pt == "-")
    }

    /// A `[...]` postfix: Index, PartSelect, or indexed part-select, opened at
    /// `mark` so the node wraps the base expression.
    fn parse_select(&mut self, mark: Mark) {
        let kind = self.select_kind();
        self.builder.start_node_at(mark, kind);
        self.bump(); // [
        if kind == SyntaxKind::IndexedPartSelect {
            // `base +: width` / `base -: width`: parse the base with a
            // precedence above `+`/`-` so the sign stays for the
            // indexed operator.
            self.parse_expr_prec(13);
            self.bump(); // `+` or `-`
            self.expect_punct(':', "in the indexed part-select");
            self.parse_expression(); // width
        } else {
            self.parse_expression();
            if kind == SyntaxKind::PartSelect {
                self.expect_punct(':', "in part-select");
                self.parse_expression();
            }
        }
        self.expect_punct(']', "to close the select");
        self.builder.finish_node();
    }

    /// Function call — wraps the callee name: Call(Name, args...).
    fn parse_call_tail(&mut self, mark: Mark) {
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
    }

    /// One call argument: an expression, or a named argument `.name[(expr)]`.
    pub(super) fn parse_call_arg(&mut self) {
        if self.at_punct('.') {
            self.builder.start_node(SyntaxKind::PortConn);
            self.bump();
            self.parse_conn_name_and_value("to close the named argument");
            self.builder.finish_node();
        } else {
            self.parse_expression();
        }
    }

    pub(super) fn parse_primary(&mut self) {
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
                self.parse_primary_literal();
            }
            TokenKind::SysTask => self.parse_primary_systask(),
            TokenKind::Ident if self.peek_text() == "new" => self.parse_primary_new(),
            TokenKind::Ident => self.parse_primary_name(),
            TokenKind::Punct if self.peek_text() == "'" && self.peek2_text() == "{" => {
                self.parse_primary_pattern();
            }
            TokenKind::Punct if self.peek_text() == "(" => {
                self.builder.start_node(SyntaxKind::ParenExpr);
                self.bump();
                self.parse_expression();
                self.expect_punct(')', "to close the parenthesized expression");
                self.builder.finish_node();
            }
            TokenKind::Punct if self.peek_text() == "{" => {
                let mark = self.builder.mark();
                self.bump(); // {
                if self.at_stream_op() {
                    self.parse_stream_concat(mark);
                    return;
                }
                self.parse_concat_or_replicate(mark);
            }
            _ => {
                self.error_here("expected an expression");
                self.bump();
            }
        }
    }

    /// A number literal; a plain (non-based) number directly followed by a
    /// time-unit identifier is an SV time literal (`1ns`, `#(2*1ns)`,
    /// `#(1 ns)`), and the unit token joins the Literal node.
    fn parse_primary_literal(&mut self) {
        let plain_number = self.peek().kind != TokenKind::StrLiteral
            && !self.peek_text().contains('\'')
            && self.at_time_unit();
        self.builder.start_node(SyntaxKind::Literal);
        self.bump();
        if plain_number {
            self.bump();
        }
        self.builder.finish_node();
    }

    /// A system function call: `$clog2(x)`, `$time`, …
    fn parse_primary_systask(&mut self) {
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

    /// `new C(42)` / `new(42)` (inside a constructor) / `new[3]`.
    fn parse_primary_new(&mut self) {
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

    /// A (hierarchical / scope-qualified) name reference: `a.b.c`, `p::x`.
    fn parse_primary_name(&mut self) {
        self.builder.start_node(SyntaxKind::Name);
        self.bump();
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

    /// Assignment pattern: `'{ a: x, b: y, default: z }`.
    fn parse_primary_pattern(&mut self) {
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

    /// A brace expression past the opening `{` (consumed by the caller):
    /// replication `{ n { expr } }` or concatenation `{ a, b, ... }`. The
    /// node must wrap from the opening brace, so the caller marked first and
    /// the node starts retroactively once the shape is known. Both closing
    /// braces of a replication belong to the node — a trailing `}` left as
    /// a sibling corrupts the parent BinExpr's operator text.
    fn parse_concat_or_replicate(&mut self, mark: Mark) {
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
}
