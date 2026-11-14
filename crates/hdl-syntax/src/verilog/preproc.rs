//! Verilog preprocessor: handles `` `define `` / `` `undef `` (object-like macros),
//! `` `ifdef `` / `` `ifndef `` / `` `else `` / `` `endif ``, and `` `include ``
//! before lexing, so the parser only ever sees plain Verilog.
//!
//! Macro references (`` `NAME ``) are expanded textually. Function-like macros are
//! not supported yet (phase 2) — a `` `NAME(...) `` reference is left in place and
//! the lexer/parser will flag it.
//!
//! Invariant: preprocessing never panics; problems are diagnostics.

use crate::diagnostic::Diagnostic;
use crate::source::{FileId, SourceFile, Span};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Result of preprocessing one file: the expanded text plus diagnostics.
pub struct PreprocResult {
    pub text: String,
    pub diags: Vec<Diagnostic>,
    /// Object-like macros defined so far (shared across `include`d files).
    pub macros: HashMap<String, String>,
}

/// Preprocess `src`. `incdirs` are searched for `` `include "file" `` in order,
/// then the directory of the including file. `file` is used for diagnostic spans.
pub fn preprocess(
    file: FileId,
    src: &SourceFile,
    incdirs: &[PathBuf],
    macros: &mut HashMap<String, String>,
    diags: &mut Vec<Diagnostic>,
) -> String {
    let mut out = String::with_capacity(src.text().len());
    let mut cond_stack: Vec<bool> = Vec::new(); // each entry: this branch is active
    let lines: Vec<&str> = src.text().split_inclusive('\n').collect();
    let mut offsets: Vec<u32> = Vec::with_capacity(lines.len());
    let mut acc = 0u32;
    for l in &lines {
        offsets.push(acc);
        acc += l.len() as u32;
    }
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let line_start = offsets[i];
        if let Some(rest) = trimmed.strip_prefix('`') {
            let (name, args) = split_ident(rest);
            let active = cond_stack.iter().all(|&a| a);
            match name.as_str() {
                "define" if active => {
                    let (mname, rest) = split_ident(args.trim_start());
                    macros.insert(mname, rest.trim().to_string());
                }
                "undef" if active => {
                    let (mname, _) = split_ident(args.trim_start());
                    macros.remove(&mname);
                }
                "ifdef" | "ifndef" if active => {
                    let (mname, _) = split_ident(args.trim_start());
                    let defined = macros.contains_key(&mname);
                    let take = if name == "ifdef" { defined } else { !defined };
                    let outer_all = cond_stack.iter().all(|&a| a);
                    cond_stack.push(take && outer_all);
                }
                "else" => {
                    let n = cond_stack.len();
                    if n > 0 {
                        // Invariant: n > 0 checked above, so last_mut cannot fail.
                        let outer_all = cond_stack[..n - 1].iter().all(|&a| a);
                        let top = cond_stack
                            .last_mut()
                            .expect("cond_stack non-empty: length checked above");
                        *top = !*top && outer_all;
                    }
                }
                "endif" => {
                    cond_stack.pop();
                }
                "include" if active => {
                    let inc = args.trim().trim_matches('"').trim();
                    let mut found: Option<PathBuf> = None;
                    for dir in incdirs {
                        let p = dir.join(inc);
                        if p.is_file() {
                            found = Some(p);
                            break;
                        }
                    }
                    if found.is_none() {
                        if let Some(parent) = Path::new(src.name()).parent() {
                            let p = parent.join(inc);
                            if p.is_file() {
                                found = Some(p);
                            }
                        }
                    }
                    match found {
                        Some(path) => match std::fs::read_to_string(&path) {
                            Ok(text) => {
                                let sub = SourceFile::new(path.display().to_string(), text);
                                let sub_id = file; // spans of included text are approximate
                                let expanded = preprocess(sub_id, &sub, incdirs, macros, diags);
                                out.push_str(&expanded);
                            }
                            Err(e) => diags.push(
                                Diagnostic::error(
                                    "E0201",
                                    Some(Span::new(
                                        file,
                                        line_start,
                                        line_start + line.len() as u32,
                                    )),
                                    format!("cannot read `include \"{}\": {}", inc, e),
                                )
                                .with_help("add the directory with -I / incdirs"),
                            ),
                        },
                        None => diags.push(
                            Diagnostic::error(
                                "E0202",
                                Some(Span::new(file, line_start, line_start + line.len() as u32)),
                                format!("`include \"{}\" not found", inc),
                            )
                            .with_help("searched incdirs and the including file's directory"),
                        ),
                    }
                }
                _ => {
                    // Unknown or inactive directive: drop the line.
                }
            }
        } else if cond_stack.iter().all(|&a| a) {
            // Expand object-like macro references in normal text.
            out.push_str(&expand_line(line, macros));
        }
    }
    out
}

fn split_ident(s: &str) -> (String, &str) {
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

fn expand_line(line: &str, macros: &HashMap<String, String>) -> String {
    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            let (name, rest) = split_ident(&line[i + 1..]);
            if let Some(value) = macros.get(&name) {
                out.push_str(value);
                i += 1 + name.len();
                let _ = rest;
                continue;
            }
        }
        // Copy one char (handles UTF-8 safely).
        let ch_len = utf8_len(bytes[i]);
        out.push_str(&line[i..i + ch_len]);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::SourceDb;

    fn run(src: &str) -> PreprocResult {
        let mut db = SourceDb::new();
        let id = db.add_file("t.v", src).unwrap();
        let mut macros = HashMap::new();
        let mut diags = Vec::new();
        let text = preprocess(id, db.file(id), &[], &mut macros, &mut diags);
        PreprocResult {
            text,
            diags,
            macros,
        }
    }

    #[test]
    fn define_expands_object_like_macro() {
        // Directive lines are consumed entirely (including their newline).
        let r = run("`define W 8\nwire [`W-1:0] x;");
        assert_eq!(r.text, "wire [8-1:0] x;");
    }

    #[test]
    fn ifdef_selects_branch() {
        let mut macros = HashMap::new();
        macros.insert("VERBOSE".to_string(), String::new());
        let src = "`ifdef VERBOSE\na\n`else\nb\n`endif\n";
        let mut db = SourceDb::new();
        let id = db.add_file("t.v", src).unwrap();
        let mut diags = Vec::new();
        let text = preprocess(id, db.file(id), &[], &mut macros, &mut diags);
        assert_eq!(text, "a\n");
    }

    #[test]
    fn unknown_macro_left_alone() {
        let r = run("wire [`UNKNOWN:0] x;");
        assert!(r.text.contains("`UNKNOWN"));
    }
}
