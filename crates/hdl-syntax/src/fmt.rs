//! Canonical source formatter for Verilog and VHDL (`hdlsim fmt`).
//!
//! Operates on the lexer token stream (the CST is lossless, so tokens carry the
//! exact source text). Strategy: preserve the author's line structure, but
//! re-derive indentation from block keywords and normalize horizontal spacing
//! between tokens. The result is idempotent: `format(format(x)) == format(x)`.
//!
//! Preprocessor directives (`` `define ``, `` `include ``) are always printed
//! at column 0 and never affect indentation.

use crate::diagnostic::Diagnostic;
use crate::language::Language;
use crate::source::SourceDb;
use crate::token::{Token, TokenKind};

/// Format `text`. Returns the formatted source plus any lexer diagnostics
/// (formatting proceeds anyway — the lexer recovers with Error tokens).
pub fn format_source(lang: Language, text: &str) -> (String, Vec<Diagnostic>) {
    let mut db = SourceDb::new();
    let id = match db.add_file("<fmt>", text) {
        Ok(id) => id,
        Err(_) => return (text.to_string(), Vec::new()),
    };
    let file = db.file(id);
    let (tokens, diags) = match lang {
        Language::Verilog => crate::verilog::lexer::lex(id, file),
        Language::Vhdl => crate::vhdl::lexer::lex(id, file),
    };
    (format_tokens(lang, text, &tokens), diags)
}

struct Item {
    text: String,
    kind: TokenKind,
}

struct Line {
    items: Vec<Item>,
    directive: bool,
}

fn push_line(lines: &mut Vec<Line>, cur: &mut Vec<Item>, blanks: &mut usize) {
    if cur.is_empty() {
        // Collapse runs of blank lines to at most one.
        if !lines.is_empty() && *blanks > 0 {
            lines.push(Line {
                items: Vec::new(),
                directive: false,
            });
        }
        *blanks = 0;
    } else {
        if !lines.is_empty() && *blanks > 0 {
            lines.push(Line {
                items: Vec::new(),
                directive: false,
            });
        }
        *blanks = 0;
        let directive = matches!(cur.first().map(|i| i.kind), Some(TokenKind::Directive));
        lines.push(Line {
            items: std::mem::take(cur),
            directive,
        });
    }
}

/// Split the token stream into lines (whitespace runs and line comments carry
/// the line breaks; block comments stay attached to their starting line).
fn split_lines(text: &str, tokens: &[Token]) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut cur: Vec<Item> = Vec::new();
    let mut blanks = 0usize;
    for t in tokens {
        let txt = &text[t.span.start as usize..t.span.end as usize];
        match t.kind {
            TokenKind::Whitespace => {
                let n = txt.matches('\n').count();
                if n > 0 {
                    push_line(&mut lines, &mut cur, &mut blanks);
                    blanks += n - 1;
                }
            }
            TokenKind::Comment => {
                let line_comment = txt.starts_with("//") || txt.starts_with("--");
                cur.push(Item {
                    text: txt.to_string(),
                    kind: t.kind,
                });
                if line_comment {
                    push_line(&mut lines, &mut cur, &mut blanks);
                } else {
                    let n = txt.matches('\n').count();
                    if n > 0 {
                        push_line(&mut lines, &mut cur, &mut blanks);
                        blanks += n - 1;
                    }
                }
            }
            TokenKind::Eof => push_line(&mut lines, &mut cur, &mut blanks),
            _ => cur.push(Item {
                text: txt.to_string(),
                kind: t.kind,
            }),
        }
    }
    push_line(&mut lines, &mut cur, &mut blanks);
    while lines.last().map(|l| l.items.is_empty()).unwrap_or(false) {
        lines.pop();
    }
    lines
}

/// Keywords that open a block (indent following lines by one level).
fn is_opener(lang: Language, w: &str) -> bool {
    match lang {
        Language::Verilog => matches!(
            w,
            "module"
                | "macromodule"
                | "begin"
                | "fork"
                | "case"
                | "casez"
                | "casex"
                | "generate"
                | "function"
                | "task"
        ),
        Language::Vhdl => matches!(
            w,
            "entity"
                | "architecture"
                | "process"
                | "package"
                | "function"
                | "procedure"
                | "if"
                | "loop"
                | "case"
                | "generate"
                | "block"
                | "record"
                | "protected"
        ),
    }
}

/// Keywords that close a block: they dedent their own line and the following.
fn is_closer(lang: Language, w: &str) -> bool {
    match lang {
        Language::Verilog => matches!(
            w,
            "end" | "endmodule" | "endcase" | "endfunction" | "endtask" | "endgenerate" | "join"
        ),
        Language::Vhdl => w == "end",
    }
}

/// Keywords that dedent their own line but keep the level (else/when arms).
fn is_print_only(lang: Language, w: &str) -> bool {
    match lang {
        Language::Verilog => w == "else",
        Language::Vhdl => matches!(w, "else" | "elsif" | "when"),
    }
}

/// Keywords after which `(` keeps a space (`if (`, `module m (` via prevprev).
fn paren_space_kw(lang: Language, w: &str) -> bool {
    match lang {
        Language::Verilog => matches!(
            w,
            "if" | "for"
                | "while"
                | "case"
                | "casez"
                | "casex"
                | "wait"
                | "forever"
                | "repeat"
                | "do"
                | "module"
                | "macromodule"
                | "function"
                | "task"
        ),
        Language::Vhdl => matches!(w, "if" | "elsif" | "while" | "wait" | "assert" | "report"),
    }
}

/// Compiler directive names (`` `define ``, `` `include ``, ...) as opposed to
/// macro references (`` `W ``).
fn is_directive_name(text: &str) -> bool {
    let name = text.strip_prefix('`').unwrap_or(text).to_ascii_lowercase();
    matches!(
        name.as_str(),
        "define"
            | "undef"
            | "include"
            | "timescale"
            | "ifdef"
            | "ifndef"
            | "elsif"
            | "else"
            | "endif"
            | "default_nettype"
            | "resetall"
            | "celldefine"
            | "endcelldefine"
            | "line"
            | "pragma"
            | "begin_keywords"
            | "end_keywords"
            | "protected"
            | "unconnected_drive"
    )
}

/// Two single-char puncts that form one multi-char operator: no space between.
fn is_op_pair(a: &str, b: &str) -> bool {
    matches!(
        (a, b),
        ("<", "=")
            | (">", "=")
            | ("=", "=")
            | ("!", "=")
            | ("&", "&")
            | ("|", "|")
            | ("=", ">")
            | ("*", "*")
            | ("<", "<")
            | (">", ">")
            | ("/", "=")
            | (":", "=")
            | ("?", "?")
            | ("-", ">")
            | ("+", "+")
            | ("-", "-")
            | ("+", "=")
            | ("-", "=")
            | ("*", "=")
            | ("/", "*")
    )
}

/// Decide whether a single space goes between `prev` and `cur`.
fn needs_space(lang: Language, prev: &Item, prevprev: Option<&Item>, cur: &Item) -> bool {
    let a = prev.text.as_str();
    let b = cur.text.as_str();
    // Macro references (`` `W ``) attach tightly to neighbors; compiler
    // directives (`` `define ``) keep normal spacing.
    if (prev.kind == TokenKind::Directive && !is_directive_name(&prev.text))
        || (cur.kind == TokenKind::Directive && !is_directive_name(&cur.text))
    {
        return false;
    }
    // No space before these closers.
    if matches!(b, "," | ";" | ")" | "]" | ".") {
        return false;
    }
    if lang == Language::Verilog && (a == ":" || b == ":") {
        return false;
    }
    if a == "@" {
        return false;
    }
    // No space after these openers.
    if matches!(a, "(" | "[" | ".") {
        return false;
    }
    // Parens: `foo(` calls, but `if (` keywords keep a space.
    if b == "(" {
        if prev.kind == TokenKind::SysTask {
            return false;
        }
        if prev.kind == TokenKind::Ident {
            let w = a.to_ascii_lowercase();
            if paren_space_kw(lang, &w) {
                return true;
            }
            return false;
        }
        return true;
    }
    // Multi-char operators from two puncts.
    if prev.kind == TokenKind::Punct && cur.kind == TokenKind::Punct {
        return !is_op_pair(a, b);
    }
    // Tick: casts (`8'(x)`) and VHDL attributes (`x'range`) attach tightly.
    if a == "'" {
        return false;
    }
    if b == "'" && matches!(prev.kind, TokenKind::Ident | TokenKind::IntLiteral) {
        return false;
    }
    // Unary `~ ! - +`: no space after when in prefix position.
    if prev.kind == TokenKind::Punct && matches!(a, "~" | "!" | "-" | "+") {
        let prefix = match prevprev {
            None => true,
            Some(p) => {
                p.kind == TokenKind::Punct
                    || p.kind == TokenKind::Directive
                    || matches!(p.text.as_str(), "," | ";")
                    || is_opener(lang, &p.text.to_ascii_lowercase())
            }
        };
        if prefix {
            return false;
        }
    }
    true
}

/// Format an already-lexed token stream.
pub fn format_tokens(lang: Language, text: &str, tokens: &[Token]) -> String {
    let lines = split_lines(text, tokens);
    let mut indent: i32 = 0;
    let mut out = String::new();
    for line in &lines {
        if line.items.is_empty() {
            out.push('\n');
            continue;
        }
        // Leading closers/print-only keywords dedent this line.
        let mut li = indent;
        for it in &line.items {
            let w = it.text.to_ascii_lowercase();
            if is_closer(lang, &w) || is_print_only(lang, &w) {
                li -= 1;
            } else {
                break;
            }
        }
        let li = li.max(0) as usize;
        if !line.directive {
            out.push_str(&"  ".repeat(li));
        }
        let mut prev: Option<&Item> = None;
        let mut prevprev: Option<&Item> = None;
        for it in &line.items {
            if let Some(p) = prev {
                if needs_space(lang, p, prevprev, it) {
                    out.push(' ');
                }
            }
            out.push_str(&it.text);
            prevprev = prev;
            prev = Some(it);
        }
        out.push('\n');
        if line.directive {
            continue;
        }
        // VHDL `if g generate` / `case x generate`: the single `end generate;`
        // closes both, so do not count the if/case opener on that line.
        let has_generate = line
            .items
            .iter()
            .any(|i| i.kind == TokenKind::Ident && i.text.eq_ignore_ascii_case("generate"));
        let mut closed = false;
        for it in &line.items {
            if it.kind != TokenKind::Ident {
                continue;
            }
            let w = it.text.to_ascii_lowercase();
            if is_closer(lang, &w) {
                indent -= 1;
                // `end entity;` / `end process;` repeat the opener keyword
                // after `end` — do not count it as a new block.
                closed = true;
            } else if !closed
                && is_opener(lang, &w)
                && !(has_generate && matches!(w.as_str(), "if" | "case"))
            {
                indent += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt_v(src: &str) -> String {
        format_source(Language::Verilog, src).0
    }

    fn fmt_h(src: &str) -> String {
        format_source(Language::Vhdl, src).0
    }

    #[test]
    fn verilog_golden_and_idempotent() {
        let messy = "module m(input clk, output reg [7:0] q);\nalways @(posedge clk) begin\nif (q == 8'hFF) q <= 0; else q <= q + 1;\nend\nendmodule\n";
        let want = "module m(input clk, output reg [7:0] q);\n  always @(posedge clk) begin\n    if (q == 8'hFF) q <= 0; else q <= q + 1;\n  end\nendmodule\n";
        let got = fmt_v(messy);
        assert_eq!(got, want, "got:\n{}", got);
        assert_eq!(fmt_v(&got), got, "not idempotent");
    }

    #[test]
    fn verilog_spacing_rules() {
        let got = fmt_v("module m;\nwire [3:0] x;\nassign y = ~x & 8'h0F;\ninitial begin $display(\"hi\"); end\nendmodule\n");
        assert!(got.contains("wire [3:0] x;"), "{}", got);
        assert!(got.contains("assign y = ~x & 8'h0F;"), "{}", got);
        assert!(got.contains("$display(\"hi\");"), "{}", got);
        assert_eq!(fmt_v(&got), got);
    }

    #[test]
    fn verilog_directives_at_column_zero_and_comments_kept() {
        let src = "`define W 8\nmodule m; // trailing\n// own line\nwire [`W-1:0] x;\nendmodule\n";
        let got = fmt_v(src);
        assert!(got.contains("`define W 8\n"), "{}", got);
        assert!(got.contains("  // own line\n"), "{}", got);
        assert!(got.contains("wire [`W-1:0] x;"), "{}", got);
        assert_eq!(fmt_v(&got), got);
    }

    #[test]
    fn vhdl_golden_and_idempotent() {
        let messy = "entity e is port(a:in bit; y:out bit); end entity;\narchitecture r of e is signal s:bit; begin\ny <= a and s;\nprocess(a) begin if a='1' then s<='0'; else s<='1'; end if; end process;\nend architecture;\n";
        let got = fmt_h(messy);
        let want = "entity e is port(a : in bit; y : out bit); end entity;\narchitecture r of e is signal s : bit; begin\n  y <= a and s;\n  process(a) begin if a = '1' then s <= '0'; else s <= '1'; end if; end process;\nend architecture;\n";
        assert_eq!(got, want, "got:\n{}", got);
        assert_eq!(fmt_h(&got), got, "not idempotent");
    }

    #[test]
    fn vhdl_generate_if_not_double_counted() {
        let src = "architecture r of e is begin\ng1: if G generate\ns <= a;\nend generate;\nend architecture;\n";
        let got = fmt_h(src);
        assert!(got.contains("  end generate;\n"), "{}", got);
        assert!(got.contains("end architecture;\n"), "{}", got);
        assert_eq!(fmt_h(&got), got);
    }

    #[test]
    fn blank_lines_collapse_and_trailing_trimmed() {
        let src = "module m;\n\n\n\nendmodule\n\n\n";
        let got = fmt_v(src);
        assert_eq!(got, "module m;\n\nendmodule\n");
    }
}
