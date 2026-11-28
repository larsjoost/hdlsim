//! Expressions: the precedence-climbing binary-operator chain (LRM
//! operator classes), unary operators (incl. VHDL-2008 `??` and the
//! reduction prefixes), primaries (numeric literals with sized
//! bit-string and physical-unit suffixes, string/char/operator-symbol
//! literals, bit-string literals, allocators, `null`, `others`, external
//! names, names, and parenthesized expressions vs aggregates), and
//! waveform values/elements with their delay mechanisms.
//!
//! Child module of `vhdl::parser`, the same pattern as `name.rs`.

use crate::kinds::SyntaxKind;
use crate::token::TokenKind;

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

impl super::Parser<'_> {
    pub fn parse_expression(&mut self) {
        self.parse_expr_prec(1);
    }

    /// Parse an expression in waveform position, where a following
    /// `when`/`else` belongs to the waveform, not to a conditional expression.
    pub(super) fn parse_waveform_value(&mut self) {
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

    pub(super) fn parse_expr_prec(&mut self, min_prec: u8) {
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

    pub(super) fn current_binop(&self) -> Option<String> {
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

    pub(super) fn parse_binops(&mut self, mark: crate::cst::Mark, min_prec: u8) {
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
    pub(super) fn eat_delay_mechanism(&mut self) {
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

    pub(super) fn parse_waveform_elements(&mut self) {
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

    pub(super) fn parse_unary(&mut self) {
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
    pub(super) fn parse_agg_choice(&mut self) {
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

    pub(super) fn parse_agg_element(&mut self) {
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

    pub(super) fn parse_primary(&mut self) {
        let t = self.peek().clone();
        match t.kind {
            TokenKind::IntLiteral | TokenKind::RealLiteral => self.parse_number_literal(),
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
            // Bit string literal: x"FF" / b"01" / o"17" / d"9".
            TokenKind::Ident if self.at_bit_string_start() => self.parse_bit_string_literal(),
            TokenKind::Ident => {
                self.parse_name();
            }
            // Paren expr or aggregate `(a, b, others => x)`.
            TokenKind::Punct if self.peek_text() == "(" => self.parse_paren_or_aggregate(),
            _ => {
                self.error_here("expected an expression");
                self.bump();
            }
        }
    }

    /// Numeric literal with optional suffixes: a sized bit-string
    /// (`32X"0"`, `32SB"1"`) or a physical unit (`100 ps`, `1.5 cm`).
    pub(super) fn parse_number_literal(&mut self) {
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
        // Sized bit-string literal with size marker: `32X"0"`, `32SB"1"`,
        // `32UB"1"` — the size integer is directly followed by an optional
        // `s` (signed) or `u` (unsigned, VHDL-2019), the base letter, and
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
        self.parse_physical_unit_suffix();
        self.builder.finish_node();
    }

    /// Physical literal suffix: `100 ps`, `1.5 cm` — a number followed by a
    /// physical unit (time, length, frequency).
    pub(super) fn parse_physical_unit_suffix(&mut self) {
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
    }

    /// Guard: an unquoted bit-string literal start — `x`/`b`/`o`/`d`
    /// directly followed by a string literal.
    pub(super) fn at_bit_string_start(&self) -> bool {
        matches!(
            self.peek_text().to_ascii_lowercase().as_str(),
            "x" | "b" | "o" | "d"
        ) && self.peek2().kind == TokenKind::StrLiteral
    }

    /// Bit string literal: x"FF" / b"01" / o"17" / d"9".
    pub(super) fn parse_bit_string_literal(&mut self) {
        self.start(SyntaxKind::Literal);
        self.bump();
        self.bump();
        self.builder.finish_node();
    }

    /// Parenthesized expression or aggregate; the mark goes before `(` so
    /// the paren is inside the node. An aggregate's first element is
    /// followed by `,`, `|`, `=>`, or a range.
    pub(super) fn parse_paren_or_aggregate(&mut self) {
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
            self.parse_aggregate_rest(mark, first_comma);
        } else {
            self.builder.start_node_at(mark, SyntaxKind::ParenExpr);
            self.expect_punct(')', "to close the parenthesized expression");
            self.builder.finish_node();
        }
    }

    /// The remaining aggregate elements after the first choice: extra
    /// choices and `=> value` for the first element, then the
    /// `,`-separated rest.
    pub(super) fn parse_aggregate_rest(&mut self, mark: crate::cst::Mark, first_comma: bool) {
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
    }
}
