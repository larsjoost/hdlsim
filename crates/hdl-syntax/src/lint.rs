//! Source linter (`hdlsim lint`): checks over text + the CST, each lint with an
//! optional safe automatic fix (`--fix`).
//!
//! Slice 1 checks:
//! - `W0001` trailing whitespace (fix: delete)
//! - `W0002` tab character (fix: expand to 4 spaces)
//! - `W0003` line longer than 100 columns
//! - `W0101` Verilog `case` without `default` arm
//! - `W0102` Verilog blocking `=` assignment in an edge-triggered `always`
//! - `W0201` VHDL `null;` statement (does nothing)

use crate::diagnostic::Severity;
use crate::kinds::SyntaxKind;
use crate::language::Language;
use crate::source::{SourceDb, Span};

/// One lint finding. `fix` is a safe in-place replacement for `span`, if any.
#[derive(Clone, Debug)]
pub struct Lint {
    pub code: &'static str,
    pub severity: Severity,
    pub span: Span,
    pub message: String,
    pub fix: Option<String>,
}

impl Lint {
    fn new(code: &'static str, severity: Severity, span: Span, message: impl Into<String>) -> Lint {
        Lint {
            code,
            severity,
            span,
            message: message.into(),
            fix: None,
        }
    }

    fn with_fix(mut self, replacement: impl Into<String>) -> Lint {
        self.fix = Some(replacement.into());
        self
    }
}

/// Run all lints over `text` (registered in `db` under `name`).
pub fn lint_source(lang: Language, db: &mut SourceDb, name: &str, text: &str) -> Vec<Lint> {
    let id = match db.add_file(name, text) {
        Ok(id) => id,
        Err(_) => return Vec::new(),
    };
    let mut lints = text_lints(id, text);
    lints.extend(structural_lints(lang, db, id));
    lints.sort_by_key(|l| (l.span.start, l.span.end));
    lints
}

/// Text-level checks: trailing whitespace, tabs, over-long lines.
fn text_lints(file: crate::source::FileId, text: &str) -> Vec<Lint> {
    let mut out = Vec::new();
    let mut off = 0usize;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        let trail = trimmed.len() - trimmed.trim_end().len();
        if trail > 0 {
            let start = off + trimmed.trim_end().len();
            out.push(
                Lint::new(
                    "W0001",
                    Severity::Warning,
                    Span::new(file, start as u32, (start + trail) as u32),
                    "trailing whitespace",
                )
                .with_fix(""),
            );
        }
        let content = trimmed.trim_end();
        if content.len() > 100 {
            out.push(Lint::new(
                "W0003",
                Severity::Warning,
                Span::new(file, off as u32, (off + content.len()) as u32),
                format!("line is {} columns (max 100)", content.len()),
            ));
        }
        let mut col = 0usize;
        for ch in content.chars() {
            if ch == '\t' {
                out.push(
                    Lint::new(
                        "W0002",
                        Severity::Warning,
                        Span::new(file, (off + col) as u32, (off + col + 1) as u32),
                        "tab character (use spaces)",
                    )
                    .with_fix("    "),
                );
            }
            col += ch.len_utf8();
        }
        off += line.len();
    }
    out
}

/// CST-level checks, per language.
fn structural_lints(lang: Language, db: &SourceDb, id: crate::source::FileId) -> Vec<Lint> {
    let file = db.file(id);
    let root = match lang {
        Language::Verilog => crate::verilog::parser::parse_file(id, file).root,
        Language::Vhdl => crate::vhdl::parser::parse_file(id, file).root,
    };
    match lang {
        Language::Verilog => {
            let mut out = Vec::new();
            for node in root.descendants() {
                match node.kind() {
                    SyntaxKind::Case | SyntaxKind::GenCase => {
                        let has_default = node
                            .children()
                            .filter(|c| c.kind() == SyntaxKind::CaseItem)
                            .any(|item| item.text(db).trim_start().starts_with("default"));
                        if !has_default {
                            out.push(Lint::new(
                                "W0101",
                                Severity::Warning,
                                node.span(),
                                "case without default arm",
                            ));
                        }
                    }
                    SyntaxKind::Always => {
                        let text = node.text(db);
                        if text.contains("posedge") || text.contains("negedge") {
                            for st in node.descendants() {
                                if st.kind() != SyntaxKind::StmtAssign {
                                    continue;
                                }
                                if let Some(span) = blocking_assign_span(&st, db) {
                                    out.push(Lint::new(
                                        "W0102",
                                        Severity::Warning,
                                        span,
                                        "blocking '=' in edge-triggered always; use '<='",
                                    ));
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            out
        }
        Language::Vhdl => root
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::NullStmt)
            .map(|n| {
                Lint::new(
                    "W0201",
                    Severity::Note,
                    n.span(),
                    "null statement does nothing",
                )
            })
            .collect(),
    }
}

/// If `stmt` (a `StmtAssign`) uses a blocking `=`, return its span.
/// `<=`, `>=`, `==`, `!=` lex as two punct leaves, so an `=` leaf whose
/// previous leaf is `<`/`>`/`!`/`=` is part of a multi-char operator.
fn blocking_assign_span(stmt: &crate::cst::SyntaxNode, db: &SourceDb) -> Option<Span> {
    let mut prev_leaf: Option<String> = None;
    for c in stmt.children() {
        let is_leaf = c.children().next().is_none();
        if is_leaf {
            let t = c.text(db);
            if t == "=" {
                let p = prev_leaf.as_deref();
                if !matches!(p, Some("<") | Some(">") | Some("!") | Some("=")) {
                    return Some(c.span());
                }
            }
            prev_leaf = Some(t.to_string());
        }
    }
    None
}

/// Apply all safe fixes (non-overlapping, latest-span first). Returns the new
/// text and how many fixes were applied.
pub fn apply_fixes(text: &str, lints: &[Lint]) -> (String, usize) {
    let mut fixes: Vec<&Lint> = lints.iter().filter(|l| l.fix.is_some()).collect();
    fixes.sort_by_key(|l| {
        (
            std::cmp::Reverse(l.span.start),
            std::cmp::Reverse(l.span.end),
        )
    });
    let mut out = text.to_string();
    let mut applied = 0;
    let mut last_start: u32 = u32::MAX;
    for l in fixes {
        if l.span.end > l.span.start && l.span.start >= last_start {
            continue; // overlap: skip
        }
        let s = l.span.start as usize;
        let e = (l.span.end as usize).min(out.len());
        if s > e || s > out.len() {
            continue;
        }
        out.replace_range(s..e, l.fix.as_deref().unwrap_or(""));
        last_start = l.span.start;
        applied += 1;
    }
    (out, applied)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lint_v(src: &str) -> Vec<Lint> {
        let mut db = SourceDb::new();
        lint_source(Language::Verilog, &mut db, "t.v", src)
    }

    fn lint_h(src: &str) -> Vec<Lint> {
        let mut db = SourceDb::new();
        lint_source(Language::Vhdl, &mut db, "t.vhd", src)
    }

    #[test]
    fn trailing_ws_tabs_and_long_lines() {
        let src = "module m;  \n\twire x;\nendmodule\n";
        let lints = lint_v(src);
        let codes: Vec<&str> = lints.iter().map(|l| l.code).collect();
        assert!(codes.contains(&"W0001"), "{:?}", codes);
        assert!(codes.contains(&"W0002"), "{:?}", codes);
        let (fixed, n) = apply_fixes(src, &lints);
        assert!(n >= 2);
        assert_eq!(fixed, "module m;\n    wire x;\nendmodule\n");
        // After fixing, no fixable lints remain.
        let again = lint_v(&fixed);
        assert!(
            again.iter().all(|l| l.fix.is_none()),
            "{:?}",
            again.iter().map(|l| l.code).collect::<Vec<_>>()
        );
    }

    #[test]
    fn case_without_default_flagged_case_with_default_ok() {
        let bad = "module m;\nalways @(*) begin\ncase (x)\n0: y = 1;\nendcase\nend\nendmodule\n";
        let lints = lint_v(bad);
        assert!(
            lints.iter().any(|l| l.code == "W0101"),
            "{:?}",
            lints.iter().map(|l| l.code).collect::<Vec<_>>()
        );
        let good = "module m;\nalways @(*) begin\ncase (x)\n0: y = 1;\ndefault: y = 0;\nendcase\nend\nendmodule\n";
        assert!(!lint_v(good).iter().any(|l| l.code == "W0101"));
    }

    #[test]
    fn blocking_assign_in_edge_always_flagged() {
        let bad = "module m;\nalways @(posedge clk) begin\nq = d;\nend\nendmodule\n";
        let lints = lint_v(bad);
        assert!(
            lints.iter().any(|l| l.code == "W0102"),
            "{:?}",
            lints.iter().map(|l| l.code).collect::<Vec<_>>()
        );
        // nonblocking and combinational always are fine
        let good = "module m;\nalways @(posedge clk) begin\nq <= d;\nend\nalways @(*) begin\nif (c == 1) y = 1;\nend\nendmodule\n";
        assert!(!lint_v(good).iter().any(|l| l.code == "W0102"));
    }

    #[test]
    fn vhdl_null_statement_flagged() {
        let src =
            "architecture r of e is begin\nprocess begin\nnull;\nend process;\nend architecture;\n";
        let lints = lint_h(src);
        assert!(
            lints.iter().any(|l| l.code == "W0201"),
            "{:?}",
            lints.iter().map(|l| l.code).collect::<Vec<_>>()
        );
    }
}
