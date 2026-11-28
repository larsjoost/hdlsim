//! Verilog preprocessor: handles `` `define `` / `` `undef `` (object-like and
//! function-like macros), `` `ifdef `` / `` `ifndef `` / `` `else `` / `` `endif ``,
//! and `` `include `` before lexing, so the parser only ever sees plain Verilog.
//!
//! Macro references (`` `NAME ``) are expanded textually. Function-like macros
//! (`` `NAME(a,b) ``) substitute formal arguments at the reference site
//! (LRM 19.3.1): commas inside brackets/braces/parens or strings do not split
//! actuals, and a reference is re-scanned until stable so macros can expand to
//! other macros — with a cap that stops self-recursive definitions.
//!
//! Invariant: preprocessing never panics; problems are diagnostics.

use crate::diagnostic::Diagnostic;
use crate::source::{FileId, SourceFile, Span};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

mod expand;

pub use expand::Macro;
use expand::{expand_text, split_ident, split_paren_group, split_top_level_args};

/// Result of preprocessing one file: the expanded text plus diagnostics.
pub struct PreprocResult {
    pub text: String,
    pub diags: Vec<Diagnostic>,
    /// Macros defined so far (shared across `include`d files).
    pub macros: HashMap<String, Macro>,
}

/// Preprocess `src`. `incdirs` are searched for `` `include "file" `` in order,
/// then the directory of the including file. `file` is used for diagnostic spans.
pub fn preprocess(
    file: FileId,
    src: &SourceFile,
    incdirs: &[PathBuf],
    macros: &mut HashMap<String, Macro>,
    diags: &mut Vec<Diagnostic>,
) -> String {
    let mut out = String::with_capacity(src.text().len());
    // Each entry: (this branch is active, some branch of this level was taken).
    let mut cond_stack: Vec<(bool, bool)> = Vec::new();
    let raw_lines: Vec<&str> = src.text().split_inclusive('\n').collect();
    let mut offsets: Vec<u32> = Vec::with_capacity(raw_lines.len());
    let mut acc = 0u32;
    for l in &raw_lines {
        offsets.push(acc);
        acc += l.len() as u32;
    }
    let lines = join_directive_lines(raw_lines);
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let line_start = offsets.get(i).copied().unwrap_or(0);
        if let Some(rest) = trimmed.strip_prefix('`') {
            let (name, args) = split_ident(rest);
            let active = cond_stack.iter().all(|&(a, _)| a);
            handle_directive_line(
                line,
                &name,
                args,
                active,
                macros,
                &mut cond_stack,
                &mut out,
                file,
                line_start,
                incdirs,
                src.name(),
                diags,
            );
        } else if cond_stack.iter().all(|&(a, _)| a) {
            // Expand macro references in normal text.
            let (expanded, converged) = expand_text(line, macros);
            if !converged {
                diags.push(macro_recursion_diag(file, line_start, line.len()));
            }
            out.push_str(&expanded);
        }
    }
    out
}

/// Process one `` ` ``-directive line: conditionals, defines, includes, and
/// macro invocations in statement position (`` `MACRO_STMT `` on its own line).
/// Unknown directives are dropped.
#[allow(clippy::too_many_arguments)]
fn handle_directive_line(
    line: &str,
    name: &str,
    args: &str,
    active: bool,
    macros: &mut HashMap<String, Macro>,
    cond_stack: &mut Vec<(bool, bool)>,
    out: &mut String,
    file: FileId,
    line_start: u32,
    incdirs: &[PathBuf],
    src_name: &str,
    diags: &mut Vec<Diagnostic>,
) {
    match name {
        "define" if active => define_macro(args, macros, file, line_start, diags),
        "undef" if active => {
            let (mname, _) = split_ident(args.trim_start());
            macros.remove(&mname);
        }
        "ifdef" | "ifndef" | "elsif" | "else" | "endif" => {
            update_cond_stack(name, args, macros, cond_stack)
        }
        "include" if active => include_file(
            args.trim().trim_matches('"').trim(),
            line,
            file,
            line_start,
            incdirs,
            src_name,
            macros,
            diags,
            out,
        ),
        "timescale" if active => {
            // Pass through: the parser records it on ParsedFile so the
            // lowerer can scale `#delay` values.
            out.push_str(line);
        }
        _ if active && macros.contains_key(name) => {
            // A macro invocation in statement position (`` `MACRO_STMT ``):
            // expand it in place — the line would otherwise be dropped.
            let (expanded, converged) = expand_text(line, macros);
            if !converged {
                diags.push(macro_recursion_diag(file, line_start, line.len()));
            }
            out.push_str(&expanded);
        }
        // Unknown or inactive directive: drop the line.
        _ => {}
    }
}

/// Conditional-compilation stack update for `` `ifdef ``/`` `ifndef ``/
/// `` `elsif ``/`` `else ``/`` `endif ``.
fn update_cond_stack(
    name: &str,
    args: &str,
    macros: &HashMap<String, Macro>,
    cond_stack: &mut Vec<(bool, bool)>,
) {
    match name {
        "ifdef" | "ifndef" => {
            let (mname, _) = split_ident(args.trim_start());
            let defined = macros.contains_key(&mname);
            let take = (if name == "ifdef" { defined } else { !defined })
                && cond_stack.iter().all(|&(a, _)| a);
            cond_stack.push((take, take));
        }
        "elsif" => {
            let n = cond_stack.len();
            if n > 0 {
                let outer_all = cond_stack[..n - 1].iter().all(|&(x, _)| x);
                let (mname, _) = split_ident(args.trim_start());
                let cond = macros.contains_key(&mname) && outer_all;
                let top = &mut cond_stack[n - 1];
                top.0 = !top.1 && cond;
                top.1 = top.1 || cond;
            }
        }
        "else" => {
            let n = cond_stack.len();
            if n > 0 {
                let outer_all = cond_stack[..n - 1].iter().all(|&(x, _)| x);
                let top = &mut cond_stack[n - 1];
                top.0 = !top.1 && outer_all;
                top.1 = true;
            }
        }
        // `endif`
        _ => {
            cond_stack.pop();
        }
    }
}

/// Record a `` `define ``. A `(` immediately after the name (no space — LRM
/// 19.3.1) introduces the formal argument list of a function-like macro.
fn define_macro(
    args: &str,
    macros: &mut HashMap<String, Macro>,
    file: FileId,
    line_start: u32,
    diags: &mut Vec<Diagnostic>,
) {
    let (mname, rest) = split_ident(args.trim_start());
    if mname.is_empty() {
        return;
    }
    if rest.starts_with('(') {
        match split_paren_group(rest) {
            Some((inside, consumed)) => {
                let formals: Vec<String> = split_top_level_args(inside)
                    .into_iter()
                    .filter(|f| !f.is_empty())
                    .collect();
                macros.insert(
                    mname,
                    Macro {
                        formals,
                        body: rest[consumed..].trim().to_string(),
                    },
                );
            }
            None => {
                // Unbalanced formal list: keep the name but treat the rest as
                // an object-like body so the file still preprocesses.
                diags.push(
                    Diagnostic::error(
                        "E0203",
                        Some(Span::new(
                            file,
                            line_start,
                            line_start + args.len() as u32 + 1,
                        )),
                        format!("malformed formal argument list in `define {}", mname),
                    )
                    .with_help("formals are plain identifiers separated by commas"),
                );
                macros.insert(
                    mname,
                    Macro {
                        formals: Vec::new(),
                        body: rest.trim().to_string(),
                    },
                );
            }
        }
    } else {
        macros.insert(
            mname,
            Macro {
                formals: Vec::new(),
                body: rest.trim().to_string(),
            },
        );
    }
}

/// Inline one `` `include "file" `` into `out`.
#[allow(clippy::too_many_arguments)]
fn include_file(
    inc: &str,
    line: &str,
    file: FileId,
    line_start: u32,
    incdirs: &[PathBuf],
    src_name: &str,
    macros: &mut HashMap<String, Macro>,
    diags: &mut Vec<Diagnostic>,
    out: &mut String,
) {
    let mut found: Option<PathBuf> = None;
    for dir in incdirs {
        let p = dir.join(inc);
        if p.is_file() {
            found = Some(p);
            break;
        }
    }
    if found.is_none() {
        if let Some(parent) = Path::new(src_name).parent() {
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
                    Some(Span::new(file, line_start, line_start + line.len() as u32)),
                    format!("cannot read `include \"{}\": {}", inc, e),
                )
                .with_help("add the directory with -I / incdirs"),
            ),
        },
        None => diags.push(
            Diagnostic::warning(
                "W0202",
                Some(Span::new(file, line_start, line_start + line.len() as u32)),
                format!("`include \"{}\" not found; skipped", inc),
            )
            .with_help("searched incdirs and the including file's directory"),
        ),
    }
}

/// Join directive lines split across physical lines: a directive name that is
/// a strict prefix of a known directive (generator wraps like "`endcellde\ndefine")
/// or an unknown directive with an unbalanced argument list is joined with the
/// following line(s).
fn join_directive_lines(raw_lines: Vec<&str>) -> Vec<String> {
    let mut lines: Vec<String> = Vec::with_capacity(raw_lines.len());
    let mut i = 0;
    while i < raw_lines.len() {
        let mut line = raw_lines[i].to_string();
        if let Some(rest) = line.trim_start().strip_prefix('`') {
            let (name, _) = split_ident(rest);
            if !name.is_empty()
                && !KNOWN_DIRECTIVES.contains(&name.as_str())
                && KNOWN_DIRECTIVES
                    .iter()
                    .any(|d| d.starts_with(&name) && *d != name)
            {
                while i + 1 < raw_lines.len() {
                    let next = raw_lines[i + 1].trim();
                    let candidate = format!("{}{}", name, next);
                    if KNOWN_DIRECTIVES.contains(&candidate.as_str()) {
                        line.push_str(next);
                        i += 1;
                        break;
                    }
                    if KNOWN_DIRECTIVES.iter().any(|d| d.starts_with(&candidate)) {
                        line.push_str(next);
                        i += 1;
                        continue;
                    }
                    break;
                }
            }
        }
        if let Some(rest) = line.trim_start().strip_prefix('`') {
            let (name, _) = split_ident(rest);
            if !name.is_empty() && !KNOWN_DIRECTIVES.contains(&name.as_str()) {
                // Unknown directive or macro invocation: join a multi-line
                // parenthesized argument list to keep the following lines from
                // leaking through.
                let mut bal = paren_balance(&line);
                while bal > 0 && i + 1 < raw_lines.len() {
                    i += 1;
                    line.push('\n');
                    line.push_str(raw_lines[i]);
                    bal += paren_balance(raw_lines[i]);
                }
            }
        }
        lines.push(line);
        i += 1;
    }
    lines
}

/// Compiler directives the preprocessor knows. Unknown ones are dropped;
/// wrapping generators may split a directive name across lines, so names that
/// are a prefix of one of these are joined with the following line(s).
const KNOWN_DIRECTIVES: &[&str] = &[
    "define",
    "undef",
    "ifdef",
    "ifndef",
    "elsif",
    "else",
    "endif",
    "include",
    "timescale",
    "celldefine",
    "endcelldefine",
    "default_nettype",
    "resetall",
    "unconnected_drive",
    "nounconnected_drive",
    "protect",
    "endprotect",
    "pragma",
    "line",
    "begin_keywords",
    "end_keywords",
];

/// Paren balance of a line, ignoring quoted strings.
fn paren_balance(s: &str) -> i32 {
    let mut bal = 0;
    let mut in_str = false;
    for c in s.chars() {
        match c {
            '"' => in_str = !in_str,
            '(' if !in_str => bal += 1,
            ')' if !in_str => bal -= 1,
            _ => {}
        }
    }
    bal
}

fn macro_recursion_diag(file: FileId, line_start: u32, line_len: usize) -> Diagnostic {
    Diagnostic::warning(
        "W0203",
        Some(Span::new(file, line_start, line_start + line_len as u32)),
        "macro expansion did not converge — recursive `define?",
    )
    .with_help("LRM 19.3.1: a macro shall not expand to itself (directly or indirectly)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::SourceDb;

    fn run(src: &str) -> PreprocResult {
        let mut db = SourceDb::new();
        let id = db.add_file("t.v", src).expect("adds file");
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
        macros.insert(
            "VERBOSE".to_string(),
            Macro {
                formals: Vec::new(),
                body: String::new(),
            },
        );
        let src = "`ifdef VERBOSE\na\n`else\nb\n`endif\n";
        let mut db = SourceDb::new();
        let id = db.add_file("t.v", src).expect("adds file");
        let mut diags = Vec::new();
        let text = preprocess(id, db.file(id), &[], &mut macros, &mut diags);
        assert_eq!(text, "a\n");
    }

    #[test]
    fn unknown_macro_left_alone() {
        let r = run("wire [`UNKNOWN:0] x;");
        assert!(r.text.contains("`UNKNOWN"));
    }

    #[test]
    fn function_like_macro_substitutes_formals() {
        let r = run("`define ADD(a,b) ((a)+(b))\ny = `ADD(a, 4'd1);\n");
        assert_eq!(r.text, "y = ((a)+(4'd1));\n");
    }

    #[test]
    fn function_like_macro_keeps_bracketed_comma() {
        // A comma inside braces belongs to one actual (LRM 19.3.1).
        let r = run("`define M(x) x\ny = `M({a, b});\n");
        assert_eq!(r.text, "y = {a, b};\n");
    }

    #[test]
    fn function_like_macro_nested_calls() {
        let r = run("`define ADD(a,b) ((a)+(b))\n`define INC(x) `ADD(x, 1)\ny = `INC(q);\n");
        assert_eq!(r.text, "y = ((q)+(1));\n");
    }

    #[test]
    fn function_like_macro_empty_actual() {
        let r = run("`define P(x) [x]\ny = `P();\n");
        assert_eq!(r.text, "y = [];\n");
    }

    #[test]
    fn function_like_macro_missing_actual_is_empty() {
        let r = run("`define P(x, y) {x, y}\ny = `P(1);\n");
        assert_eq!(r.text, "y = {1, };\n");
    }

    #[test]
    fn statement_position_macro_expands() {
        let r = run("`define MACRO_STMT assign y = a;\nmodule m;\n  `MACRO_STMT\nendmodule\n");
        assert_eq!(r.text, "module m;\n  assign y = a;\nendmodule\n");
    }

    #[test]
    fn statement_position_function_like_macro_expands() {
        let r = run("`define CHK(c) if (!(c)) $fail;\nmodule m;\n  `CHK(x == 1)\nendmodule\n");
        assert_eq!(r.text, "module m;\n  if (!(x == 1)) $fail;\nendmodule\n");
    }

    #[test]
    fn undefined_statement_macro_dropped() {
        let r = run("module m;\n  `NOT_DEFINED\nendmodule\n");
        assert_eq!(r.text, "module m;\nendmodule\n");
    }

    #[test]
    fn object_like_macro_name_takes_bracket_body_verbatim() {
        // The `(` of an object-like macro's expansion is not an argument list.
        let r = run("`define PAIR (a, b)\ny = `PAIR;\n");
        assert_eq!(r.text, "y = (a, b);\n");
    }

    #[test]
    fn formal_shadows_same_named_macro() {
        // The formal wins over a same-named macro (substitution first).
        let r = run("`define B 5\n`define F(B) B+1\ny = `F(2);\n");
        assert_eq!(r.text, "y = 2+1;\n");
    }

    #[test]
    fn splice_removes_backtick_backtick() {
        let r = run("`define CAT(a, b) a``b\ny = `CAT(pre, post);\n");
        assert_eq!(r.text, "y = prepost;\n");
    }

    #[test]
    fn recursive_macro_stops_with_diagnostic() {
        // Direct recursion: the body is its own reference.
        let r = run("`define R `R\nx = `R;\n");
        assert!(r.text.contains("`R"), "reference left in place: {}", r.text);
        assert!(
            r.diags.iter().any(|d| d.code == "W0203"),
            "recursion reported: {:?}",
            r.diags
        );
    }

    #[test]
    fn growing_recursive_macro_hits_cap_with_diagnostic() {
        let r = run("`define R x`R\nx = `R;\n");
        assert!(
            r.diags.iter().any(|d| d.code == "W0203"),
            "recursion reported: {:?}",
            r.diags
        );
    }

    #[test]
    fn function_like_macro_not_expanded_without_args() {
        // Missing argument list: the reference stays for the parser to flag.
        let r = run("`define ADD(a,b) ((a)+(b))\ny = `ADD;\n");
        assert!(r.text.contains("`ADD"), "{}", r.text);
    }

    #[test]
    fn define_inside_inactive_ifdef_not_stored() {
        let r = run("`ifdef NOPE\n`define W 16\n`endif\nwire [`W:0] x;\n");
        assert!(r.text.contains("`W"), "{}", r.text);
    }

    #[test]
    fn macro_defined_after_use_in_later_line_expands() {
        let r = run("`define A `B\n`define B 1\nx = `A;\n");
        assert_eq!(r.text, "x = 1;\n");
    }
}
