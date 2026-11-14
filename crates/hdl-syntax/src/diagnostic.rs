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

    /// rustc-style rendering: `file:line:col: error[E0001]: msg` plus a source
    /// snippet with a caret line, and an optional `help:` line.
    pub fn render(&self, db: &SourceDb) -> String {
        let mut out = String::new();
        match self.span {
            Some(span) => {
                let file = db.file(span.file);
                let (line, col) = file.line_col(span.start);
                let _ = write!(
                    out,
                    "{}:{}:{}: {}[{}]: {}",
                    file.name(),
                    line,
                    col,
                    self.severity.name(),
                    self.code,
                    self.message
                );
                let text = db.line_of(span.file, span.start);
                let caret_len = span.len().max(1);
                let _ = write!(
                    out,
                    "\n  {} | {}\n    | {}^ {}",
                    line,
                    text,
                    " ".repeat((col - 1) as usize),
                    "~".repeat(caret_len as usize - 1)
                );
            }
            None => {
                let _ = write!(
                    out,
                    "{}[{}]: {}",
                    self.severity.name(),
                    self.code,
                    self.message
                );
            }
        }
        if let Some(help) = &self.help {
            let _ = write!(out, "\n  help: {}", help);
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
        self.items
            .iter()
            .map(|d| d.render(db))
            .collect::<Vec<_>>()
            .join("\n")
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
        assert!(
            out.contains("t.v:2:4: error[E0001]: missing semicolon"),
            "got: {}",
            out
        );
        assert!(out.contains("  wire x"), "got: {}", out);
        assert!(out.contains("help: add `;` here"), "got: {}", out);
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
