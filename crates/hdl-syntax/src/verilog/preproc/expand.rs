//! Macro expansion engine for the preprocessor: reference scanning, formal
//! argument substitution, and the convergence loop that lets macros expand to
//! other macros while capping self-recursion.

use std::collections::HashMap;
/// A macro definition: object-like (no formals) or function-like.
#[derive(Clone, Debug)]
pub struct Macro {
    /// Formal argument names, in order. Empty for object-like macros.
    pub formals: Vec<String>,
    /// Replacement text (formals are substituted at each reference).
    pub body: String,
}

pub(crate) fn split_ident(s: &str) -> (String, &str) {
    let mut end = 0;
    for (i, ch) in s.char_indices() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '$' {
            end = i + ch.len_utf8();
        } else {
            break;
        }
    }
    (s[..end].to_string(), &s[end..])
}
/// Expand macro references in `text` until stable. Returns the expanded text
/// and whether expansion converged (false when a definition expands to its own
/// reference or the growth cap was hit — LRM 19.3.1 forbids recursive macros).
pub(crate) fn expand_text(text: &str, macros: &HashMap<String, Macro>) -> (String, bool) {
    let mut cur = text.to_string();
    for _ in 0..EXPANSION_CAP {
        let (next, substituted) = expand_once(&cur, macros);
        if next == cur {
            // A substitution that left the text unchanged means the body
            // expanded to its own reference (`` `define R `R ``).
            return (cur, !substituted);
        }
        cur = next;
    }
    (cur, false)
}
/// Expansion rounds before a self-recursive macro is reported and left as-is.
const EXPANSION_CAP: usize = 100;
/// One expansion pass: replace every `` `NAME `` (with actuals for
/// function-like macros) by its body. Returns the new text and whether any
/// reference was substituted.
fn expand_once(text: &str, macros: &HashMap<String, Macro>) -> (String, bool) {
    let mut out = String::with_capacity(text.len());
    let mut substituted = false;
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            let (name, rest) = split_ident(&text[i + 1..]);
            if !name.is_empty() {
                if let Some(mac) = macros.get(&name) {
                    if mac.formals.is_empty() {
                        out.push_str(&mac.body);
                        i += 1 + name.len();
                        substituted = true;
                        continue;
                    }
                    if let Some((args_txt, consumed)) = split_paren_group(rest) {
                        let actuals = split_top_level_args(args_txt);
                        out.push_str(&substitute_formals(&mac.body, &mac.formals, &actuals));
                        i += 1 + name.len() + consumed;
                        substituted = true;
                        continue;
                    }
                    // Function-like macro without its argument list: leave the
                    // reference in place for the parser to flag.
                }
            }
        }
        // Copy one char (handles UTF-8 safely).
        let ch_len = utf8_len(bytes[i]);
        out.push_str(&text[i..i + ch_len]);
        i += ch_len;
    }
    (out, substituted)
}
/// Split a parenthesized group at the start of `s`: returns `(inside, bytes
/// consumed including both parens)`, or `None` when the group does not close
/// on this line. Quoted strings are skipped (escaped quotes inside macro
pub(crate) fn split_paren_group(s: &str) -> Option<(&str, usize)> {
    let bytes = s.as_bytes();
    if bytes.first() != Some(&b'(') {
        return None;
    }
    let mut depth = 0usize;
    let mut in_str = false;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'"' => in_str = !in_str,
            _ if in_str => {}
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some((&s[1..i], i + 1));
                }
            }
            _ => {}
        }
    }
    None
}
/// Split a macro actual-argument list on top-level commas: commas inside
/// `()`, `[]`, `{}` or quoted strings do not split (LRM 19.3.1). An empty list
pub(crate) fn split_top_level_args(s: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    let mut in_str = false;
    for ch in s.chars() {
        match ch {
            '"' => {
                in_str = !in_str;
                cur.push(ch);
            }
            _ if in_str => cur.push(ch),
            '(' | '[' | '{' => {
                depth += 1;
                cur.push(ch);
            }
            ')' | ']' | '}' => {
                depth -= 1;
                cur.push(ch);
            }
            ',' if depth == 0 => {
                args.push(cur.trim().to_string());
                cur = String::new();
            }
            _ => cur.push(ch),
        }
    }
    args.push(cur.trim().to_string());
    args
}
/// Substitute actual arguments for formal names in a macro body, then remove
pub(crate) fn substitute_formals(body: &str, formals: &[String], actuals: &[String]) -> String {
    let mut out = String::with_capacity(body.len());
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'`' && bytes.get(i + 1) == Some(&b'`') {
            i += 2;
            continue;
        }
        let (name, _) = split_ident(&body[i..]);
        if !name.is_empty() {
            if let Some(pos) = formals.iter().position(|f| f == &name) {
                out.push_str(actuals.get(pos).map(String::as_str).unwrap_or(""));
                i += name.len();
                continue;
            }
        }
        let ch_len = utf8_len(bytes[i]);
        out.push_str(&body[i..i + ch_len]);
        i += ch_len;
    }
    out
}

fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else {
        4
    }
}
