//! SV streaming operator parsing: `{>>{ expr }}` / `{<<W{ expr }}` (LRM
//! 11.4.14) — one StreamConcat node wrapping the optional slice width and the
//! brace-wrapped operand list. `<<`/`>>` lex as two single-char puncts, so
//! the shape is detected at the `{` primary arm before replicate/concat.
//! Child module of `verilog::parser`, the same pattern as `sva.rs`.

use crate::cst::Mark;
use crate::kinds::SyntaxKind;

impl super::Parser<'_> {
    /// True when the cursor (just past a `{`) begins a stream operator:
    /// `<<` or `>>`, each lexed as two single-char puncts.
    pub(super) fn at_stream_op(&self) -> bool {
        (self.at_punct('<') && self.peek2_text() == "<")
            || (self.at_punct('>') && self.peek2_text() == ">")
    }

    /// Parse `{>>{...}}` / `{<<W{...}}` after the opening brace: the
    /// operator, an optional slice width (absent = 1, bit stream), the
    /// brace-wrapped operand list, and both closing braces. `mark` is the
    /// opening `{`'s position so the node wraps from it.
    pub(super) fn parse_stream_concat(&mut self, mark: Mark) {
        self.bump(); // < or >
        self.bump(); // < or >
                     // Optional slice width before the inner `{` — absent
                     // means 1 (bit stream).
        if !self.at_punct('{') {
            self.parse_expression();
        }
        self.expect_punct('{', "to open the streamed operand");
        self.parse_expression();
        while self.eat_punct(',') {
            self.parse_expression();
        }
        self.expect_punct('}', "to close the streamed operand");
        self.expect_punct('}', "to close the stream operator");
        self.builder.start_node_at(mark, SyntaxKind::StreamConcat);
        self.builder.finish_node();
    }
}
