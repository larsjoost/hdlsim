//! Unit tests for the Verilog lexer: token shapes for literals, strings,
//! directives, comments, and error recovery. Child module of
//! `verilog::lexer` (house pattern: tests stay colocated with the code).

use super::*;

/// Tokens carry only spans, so the SourceFile can be dropped after lexing.
fn lex_str(src: &str) -> (Vec<Token>, Vec<Diagnostic>) {
    let file = SourceFile::new("t.v", src);
    lex(FileId(0), &file)
}

fn kinds(src: &str) -> Vec<TokenKind> {
    lex_str(src).0.iter().map(|t| t.kind).collect()
}

#[test]
fn lexes_module_header() {
    let ks = kinds("module m;");
    assert_eq!(
        ks,
        vec![
            TokenKind::Ident,
            TokenKind::Whitespace,
            TokenKind::Ident,
            TokenKind::Punct,
            TokenKind::Eof
        ]
    );
}

#[test]
fn lexes_comments_and_strings() {
    let ks = kinds("// hi\n/* x */ \"a\\\"b\"");
    assert_eq!(
        ks,
        vec![
            TokenKind::Comment,
            TokenKind::Whitespace,
            TokenKind::Comment,
            TokenKind::Whitespace,
            TokenKind::StrLiteral,
            TokenKind::Eof
        ]
    );
}

#[test]
fn lexes_sys_task_directive_and_literals() {
    let ks = kinds("$display(`WIDTH, 16'hFF, 'b10, 3.14, 1e-3)");
    assert_eq!(
        ks,
        vec![
            TokenKind::SysTask,
            TokenKind::Punct,
            TokenKind::Directive,
            TokenKind::Punct,
            TokenKind::Whitespace,
            TokenKind::IntLiteral,
            TokenKind::Punct,
            TokenKind::Whitespace,
            TokenKind::IntLiteral,
            TokenKind::Punct,
            TokenKind::Whitespace,
            TokenKind::RealLiteral,
            TokenKind::Punct,
            TokenKind::Whitespace,
            TokenKind::RealLiteral,
            TokenKind::Punct,
            TokenKind::Eof
        ]
    );
}

#[test]
fn token_spans_reconstruct_source() {
    let src = "module m; // c\nendmodule";
    let (tokens, diags) = lex_str(src);
    assert!(diags.is_empty());
    let mut recon = String::new();
    let file = SourceFile::new("t.v", src);
    for t in &tokens {
        if t.kind == TokenKind::Eof {
            continue;
        }
        recon.push_str(&file.text()[t.span.start as usize..t.span.end as usize]);
    }
    assert_eq!(recon, src);
}

#[test]
fn unterminated_string_is_a_diagnostic_not_a_panic() {
    let (_, diags) = lex_str("\"oops");
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("unterminated string"));
}

#[test]
fn trailing_backslash_in_string_stays_in_bounds() {
    // `"\\` — the escape consumes the byte after the backslash even at
    // EOF; the token span must not run past the end of the source.
    let src = "\"\\\\";
    let (tokens, diags) = lex_str(src);
    assert!(diags
        .iter()
        .any(|d| d.message.contains("unterminated string")));
    for t in &tokens {
        assert!(t.span.start as usize <= t.span.end as usize);
        assert!(t.span.end as usize <= src.len());
    }
    let str_tok = tokens
        .iter()
        .find(|t| t.kind == TokenKind::StrLiteral)
        .expect("string token");
    assert_eq!(str_tok.span.end as usize, src.len());
}

#[test]
fn lexes_escaped_identifiers_and_unsized_literals() {
    // `\a+b` is one escaped identifier (no whitespace inside); the
    // terminating whitespace is a separate token.
    let ks = kinds("\\a+b = '0 + '1 + 'x;");
    assert_eq!(
        ks,
        vec![
            TokenKind::Ident, // \a+b
            TokenKind::Whitespace,
            TokenKind::Punct, // =
            TokenKind::Whitespace,
            TokenKind::IntLiteral, // '0
            TokenKind::Whitespace,
            TokenKind::Punct, // +
            TokenKind::Whitespace,
            TokenKind::IntLiteral, // '1
            TokenKind::Whitespace,
            TokenKind::Punct, // +
            TokenKind::Whitespace,
            TokenKind::IntLiteral, // 'x
            TokenKind::Punct,      // ;
            TokenKind::Eof
        ]
    );
    // The cast tick is punctuation, not a literal start.
    let ks = kinds("signed'(x) 8'(y)");
    assert!(ks.contains(&TokenKind::Punct));
    assert!(!ks.contains(&TokenKind::Error));
}

#[test]
fn never_panics_on_arbitrary_bytes() {
    // Fuzz-lite: every single byte and some slices must lex without panic.
    for b in 0u16..=255 {
        let _ = lex_str(&String::from_utf8_lossy(&[b as u8]));
    }
    let _ = lex_str("\u{1F600} \u{0} $ ` ' \" /*");
}
