//! Diagnostics: errors and warnings as values.
//!
//! Invariant: nothing in the parser or lexer panics on user input — every problem
//! is a [`Diagnostic`]. Codes are stable strings (`E0001`, `W0001`, ...) registered
//! centrally so `--help <code>` can explain them; new checks add codes without
//! touching existing ones.

use crate::source::{SourceDb, Span};
use std::fmt::Write as _;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Severity {
    Error,
    Warning,
    Note,
}

impl Severity {
    pub fn name(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Note => "note",
        }
    }
}

/// One diagnostic. `span` is `None` for file-level messages.
#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub code: &'static str,
    pub severity: Severity,
    pub span: Option<Span>,
    pub message: String,
    pub help: Option<String>,
}

impl Diagnostic {
    pub fn error(code: &'static str, span: Option<Span>, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            code,
            severity: Severity::Error,
            span,
            message: message.into(),
            help: None,
        }
    }

    pub fn warning(
        code: &'static str,
        span: Option<Span>,
        message: impl Into<String>,
    ) -> Diagnostic {
        Diagnostic {
            code,
            severity: Severity::Warning,
            span,
            message: message.into(),
            help: None,
        }
    }

    pub fn with_help(mut self, help: impl Into<String>) -> Diagnostic {
        self.help = Some(help.into());
        self
    }

    /// rustc-style rendering:
    /// ```text
    /// error[E0001]: missing semicolon
    ///   --> t.vhd:2:4
    ///    |
    ///  2 |   wire x
    ///    |      ^^^
    ///    |
    ///    = help: add `;` here
    /// ```
    pub fn render(&self, db: &SourceDb) -> String {
        let mut out = String::new();
        let _ = write!(
            out,
            "{}[{}]: {}",
            self.severity.name(),
            self.code,
            self.message
        );
        if let Some(span) = self.span {
            let file = db.file(span.file);
            let (line, col) = file.line_col(span.start);
            let w = line.to_string().len(); // gutter width = line-number digits
            let _ = write!(
                out,
                "\n{}--> {}:{}:{}",
                " ".repeat(w + 1),
                file.name(),
                line,
                col
            );
            // Source line with tabs expanded (caret column is computed on the
            // expanded text so it stays aligned).
            let raw = db.line_of(span.file, span.start);
            let text = raw.replace('\t', "    ");
            let disp_col = {
                let mut c = 1usize;
                for ch in raw.chars().take((col - 1) as usize) {
                    c += if ch == '\t' { 4 } else { 1 };
                }
                c
            };
            let _ = write!(out, "\n{} |", " ".repeat(w + 1));
            let _ = write!(out, "\n {:>w$} | {}", line, text, w = w);
            // Caret run: span clamped to the first line.
            let line_end = span.start as usize + raw.len();
            let caret_end = (span.end as usize)
                .min(line_end)
                .max(span.start as usize + 1);
            let caret_len = (caret_end - span.start as usize).max(1);
            let _ = write!(
                out,
                "\n{} | {}{}",
                " ".repeat(w + 1),
                " ".repeat(disp_col - 1),
                "^".repeat(caret_len)
            );
            let _ = write!(out, "\n{} |", " ".repeat(w + 1));
            if let Some(help) = &self.help {
                let _ = write!(out, "\n{} = help: {}", " ".repeat(w + 1), help);
            }
        }
        out
    }
}

/// An ordered collection of diagnostics. Iteration is deterministic: insertion
/// order, which parse stages produce in source order.
#[derive(Default, Debug)]
pub struct Diagnostics {
    pub items: Vec<Diagnostic>,
}

impl Diagnostics {
    pub fn new() -> Diagnostics {
        Diagnostics { items: Vec::new() }
    }

    pub fn push(&mut self, d: Diagnostic) {
        self.items.push(d);
    }

    pub fn extend(&mut self, other: impl IntoIterator<Item = Diagnostic>) {
        self.items.extend(other);
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn has_errors(&self) -> bool {
        self.items.iter().any(|d| d.severity == Severity::Error)
    }

    pub fn render_all(&self, db: &SourceDb) -> String {
        let mut out = self
            .items
            .iter()
            .map(|d| d.render(db))
            .collect::<Vec<_>>()
            .join("\n\n");
        // rustc-style summary footer.
        let errors = self
            .items
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .count();
        let warnings = self
            .items
            .iter()
            .filter(|d| d.severity == Severity::Warning)
            .count();
        if errors > 0 {
            out.push_str(&format!(
                "\nerror: could not compile due to {} previous error{}",
                errors,
                if errors == 1 { "" } else { "s" }
            ));
        }
        if warnings > 0 {
            out.push_str(&format!(
                "\nwarning: {} warning{} generated",
                warnings,
                if warnings == 1 { "" } else { "s" }
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::FileId;

    #[test]
    fn renders_location_and_caret() {
        let mut db = SourceDb::new();
        db.add_file("t.v", "module m;\n  wire x\nendmodule\n")
            .unwrap();
        let d = Diagnostic::error(
            "E0001",
            Some(Span::new(FileId(0), 13, 16)),
            "missing semicolon",
        )
        .with_help("add `;` here");
        let out = d.render(&db);
        // Exact rustc layout: header, --> line, gutter, source, carets, help.
        let expected = "error[E0001]: missing semicolon\n  --> t.v:2:4\n   |\n 2 |   wire x\n   |    ^^^\n   |\n   = help: add `;` here";
        assert_eq!(out, expected);
    }

    #[test]
    fn render_all_appends_rustc_style_summary() {
        let mut db = SourceDb::new();
        db.add_file("t.v", "module m;\nendmodule\n").unwrap();
        let mut ds = Diagnostics::new();
        ds.push(Diagnostic::error("E0001", None, "e1"));
        ds.push(Diagnostic::error("E0002", None, "e2"));
        ds.push(Diagnostic::warning("W0001", None, "w"));
        let out = ds.render_all(&db);
        assert!(
            out.contains("error: could not compile due to 2 previous errors"),
            "{}",
            out
        );
        assert!(out.contains("warning: 1 warning generated"), "{}", out);
    }

    #[test]
    fn has_errors_tracks_severity() {
        let mut ds = Diagnostics::new();
        assert!(!ds.has_errors());
        ds.push(Diagnostic::warning("W0001", None, "w"));
        assert!(!ds.has_errors());
        ds.push(Diagnostic::error("E0001", None, "e"));
        assert!(ds.has_errors());
    }
}
