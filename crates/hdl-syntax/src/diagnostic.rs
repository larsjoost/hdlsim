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

/// Terminal color mode for diagnostic rendering.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColorMode {
    /// Color when stderr is a terminal; disabled by `NO_COLOR`, overridable
    /// with `HDLSIM_COLOR=always|never`.
    Auto,
    Always,
    Never,
}

static COLOR_MODE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Set the global color mode (e.g. from a `--color` CLI flag). Explicitly set
/// modes win over the `HDLSIM_COLOR`/`NO_COLOR` environment.
pub fn set_color_mode(mode: ColorMode) {
    let v = match mode {
        ColorMode::Auto => 0,
        ColorMode::Always => 1,
        ColorMode::Never => 2,
    };
    COLOR_MODE.store(v, std::sync::atomic::Ordering::Relaxed);
}

pub fn color_mode() -> ColorMode {
    match COLOR_MODE.load(std::sync::atomic::Ordering::Relaxed) {
        1 => ColorMode::Always,
        2 => ColorMode::Never,
        _ => ColorMode::Auto,
    }
}

/// Should diagnostics be colorized right now?
pub fn color_enabled() -> bool {
    match color_mode() {
        ColorMode::Always => true,
        ColorMode::Never => false,
        ColorMode::Auto => {
            use std::io::IsTerminal;
            if std::env::var_os("NO_COLOR").is_some() {
                return false;
            }
            match std::env::var("HDLSIM_COLOR") {
                Ok(v) if v == "always" || v == "1" || v == "true" => return true,
                Ok(v) if v == "never" || v == "0" || v == "false" => return false,
                _ => {}
            }
            std::io::stderr().is_terminal()
        }
    }
}

/// Wrap `s` in an ANSI SGR code when color is on.
pub fn paint(on: bool, code: &str, s: &str) -> String {
    if on {
        format!("\x1b[{}m{}\x1b[0m", code, s)
    } else {
        s.to_string()
    }
}

/// SGR code for a severity: error bold red, warning bold yellow, note bold blue.
fn sev_code(sev: Severity) -> &'static str {
    match sev {
        Severity::Error => "1;31",
        Severity::Warning => "1;33",
        Severity::Note => "1;34",
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

    /// Binary encoding of one diagnostic (spans stored as offsets; the `FileId`
    /// is re-attached by [`decode_diags`]).
    fn encode(&self, out: &mut Vec<u8>) {
        out.push(match self.severity {
            Severity::Error => 0,
            Severity::Warning => 1,
            Severity::Note => 2,
        });
        w_str(out, self.code);
        match self.span {
            Some(s) => {
                out.push(1);
                out.extend_from_slice(&s.start.to_le_bytes());
                out.extend_from_slice(&s.end.to_le_bytes());
            }
            None => out.push(0),
        }
        w_str(out, &self.message);
        match &self.help {
            Some(h) => {
                out.push(1);
                w_str(out, h);
            }
            None => out.push(0),
        }
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
    /// Render with the global color mode (see [`color_enabled`]).
    pub fn render(&self, db: &SourceDb) -> String {
        self.render_with(db, color_enabled())
    }

    /// Render with explicit color on/off.
    pub fn render_with(&self, db: &SourceDb, color: bool) -> String {
        let mut out = String::new();
        let head = format!("{}[{}]:", self.severity.name(), self.code);
        let _ = write!(
            out,
            "{} {}",
            paint(color, sev_code(self.severity), &head),
            self.message
        );
        if let Some(span) = self.span {
            let file = db.file(span.file);
            let (line, col) = file.line_col(span.start);
            let w = line.to_string().len(); // gutter width = line-number digits
            let loc = format!("--> {}:{}:{}", file.name(), line, col);
            let _ = write!(out, "\n{}{}", " ".repeat(w + 1), paint(color, "1", &loc));
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
                paint(color, sev_code(self.severity), &"^".repeat(caret_len))
            );
            let _ = write!(out, "\n{} |", " ".repeat(w + 1));
            if let Some(help) = &self.help {
                let _ = write!(
                    out,
                    "\n{} {}",
                    " ".repeat(w + 1),
                    paint(color, "1;36", &format!("= help: {}", help))
                );
            }
        }
        out
    }
}

fn w_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u32).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

/// Known diagnostic codes (interned as `&'static str` on decode; unknown codes
/// from a corrupt cache collapse to "E0000").
const KNOWN_CODES: &[&str] = &[
    "E0001", "E0002", "E0101", "E0102", "E0103", "E0104", "E0105", "E0106", "E0201", "E0202",
    "E0301", "E0401", "E0501", "W0001", "W0401",
];

fn intern_code(s: &str) -> &'static str {
    KNOWN_CODES
        .iter()
        .find(|c| **c == s)
        .copied()
        .unwrap_or("E0000")
}

/// Explanations for every code in [`KNOWN_CODES`] plus the lint codes emitted
/// by [`crate::lint`]. Backs `hdlsim explain <code>`.
const CODE_EXPLAIN: &[(&str, &str)] = &[
    ("E0000", "placeholder substituted for an unrecognized code (e.g. from a corrupt cache entry); re-run with HDLSIM_NO_CACHE=1 to see the real code"),
    ("E0001", "generic error (reserved)"),
    ("E0002", "generic error (reserved)"),
    ("E0101", "lexer: unexpected character (a non-ASCII byte in Verilog sources; VHDL uses this code for other invalid characters)"),
    ("E0102", "unterminated block comment; add `*/` to close it"),
    ("E0103", "unterminated string literal; add a closing quote"),
    ("E0104", "`$` must be followed by a system task name (e.g. $display, $finish)"),
    ("E0105", "backtick must be followed by a compiler directive name (e.g. `define, `include)"),
    ("E0106", "expected a base specifier after `'` in a based literal (valid bases: 'b 'o 'd 'h, optionally signed: 'sb ...)"),
    ("E0201", "cannot read a `include file (I/O error); check the path and permissions, and add its directory with -I"),
    ("E0202", "`include file not found; searched the include dirs (-I) and the including file's directory"),
    ("E0301", "parser: a token was expected here (commonly `;`); the message names the exact expected token"),
    ("E0401", "lowering/semantic error, e.g. unknown identifier or port without a name; see the message"),
    ("E0501", "library manager file I/O error: a source file could not be read; check the path and permissions"),
    ("W0001", "lint: trailing whitespace (auto-fixable with hdlsim lint --fix)"),
    ("W0002", "lint: tab character; use spaces instead (auto-fixable with hdlsim lint --fix)"),
    ("W0003", "lint: line longer than 100 columns"),
    ("W0101", "lint: case statement without a default arm"),
    ("W0102", "lint: blocking '=' assignment inside an edge-triggered always block; use '<='"),
    ("W0201", "lint: VHDL null statement does nothing"),
    ("W0401", "VHDL lowering warning, e.g. an unsupported construct degraded to a no-op; see the message"),
    ("W0501", "analyze: signal is never driven anywhere in its module, so it stays X; check for a missing assignment or port connection"),
    ("W0502", "analyze: signal is never read (dead logic); remove it or wire it to an output/port"),
];

/// Explain a diagnostic or lint code (`hdlsim explain E0301`). Covers every
/// code in [`KNOWN_CODES`] plus the lint codes from [`crate::lint`].
pub fn explain_code(code: &str) -> Option<&'static str> {
    CODE_EXPLAIN
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, e)| *e)
}

/// All explainable codes with their texts, in table order.
pub fn explained_codes() -> impl Iterator<Item = (&'static str, &'static str)> {
    CODE_EXPLAIN.iter().copied()
}

/// Serialize a diagnostic list (little-endian, length-prefixed).
pub fn encode_diags(ds: &[Diagnostic]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(ds.len() as u32).to_le_bytes());
    for d in ds {
        d.encode(&mut out);
    }
    out
}

/// Decode [`encode_diags`] output, re-attaching `file` to every span.
pub fn decode_diags(file: crate::source::FileId, bytes: &[u8]) -> Option<Vec<Diagnostic>> {
    struct R<'a> {
        buf: &'a [u8],
        pos: usize,
    }
    impl<'a> R<'a> {
        fn take(&mut self, n: usize) -> Option<&'a [u8]> {
            let s = self.buf.get(self.pos..self.pos.checked_add(n)?)?;
            self.pos += n;
            Some(s)
        }
        fn u8(&mut self) -> Option<u8> {
            Some(self.take(1)?[0])
        }
        fn u32(&mut self) -> Option<u32> {
            Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
        }
        fn str(&mut self) -> Option<&'a str> {
            let n = self.u32()? as usize;
            std::str::from_utf8(self.take(n)?).ok()
        }
    }
    let mut r = R { buf: bytes, pos: 0 };
    let n = r.u32()? as usize;
    if n > (1 << 20) {
        return None;
    }
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let severity = match r.u8()? {
            0 => Severity::Error,
            1 => Severity::Warning,
            2 => Severity::Note,
            _ => return None,
        };
        let code = intern_code(r.str()?);
        let span = match r.u8()? {
            0 => None,
            1 => Some(crate::source::Span::new(file, r.u32()?, r.u32()?)),
            _ => return None,
        };
        let message = r.str()?.to_string();
        let help = match r.u8()? {
            0 => None,
            1 => Some(r.str()?.to_string()),
            _ => return None,
        };
        out.push(Diagnostic {
            code,
            severity,
            span,
            message,
            help,
        });
    }
    if r.pos != bytes.len() {
        return None;
    }
    Some(out)
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
        self.render_all_with(db, color_enabled())
    }

    /// Render all diagnostics with explicit color on/off.
    pub fn render_all_with(&self, db: &SourceDb, color: bool) -> String {
        let mut out = self
            .items
            .iter()
            .map(|d| d.render_with(db, color))
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
                "\n{}",
                paint(
                    color,
                    "1;31",
                    &format!(
                        "error: could not compile due to {} previous error{}",
                        errors,
                        if errors == 1 { "" } else { "s" }
                    )
                )
            ));
        }
        if warnings > 0 {
            out.push_str(&format!(
                "\n{}",
                paint(
                    color,
                    "1;33",
                    &format!(
                        "warning: {} warning{} generated",
                        warnings,
                        if warnings == 1 { "" } else { "s" }
                    )
                )
            ));
        }
        // Terminate the last line so callers printing with `eprint!` do not
        // glue the next output onto the summary.
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
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
    fn render_colorizes_by_severity_when_enabled() {
        let mut db = SourceDb::new();
        db.add_file("t.v", "module m;\n  wire x\nendmodule\n")
            .unwrap();
        let mut ds = Diagnostics::new();
        ds.push(Diagnostic::error(
            "E0301",
            Some(crate::source::Span::new(crate::source::FileId(0), 12, 17)),
            "expected `;`",
        ));
        ds.push(Diagnostic::warning("W0001", None, "careful"));
        // Use the explicit on/off variants so the test does not race on the
        // global color mode with other tests.
        let plain = ds.render_all_with(&db, false);
        assert!(!plain.contains("\x1b["), "{}", plain);
        let colored = ds.render_all_with(&db, true);
        assert!(
            colored.contains("\x1b[1;31merror[E0301]:\x1b[0m"),
            "{}",
            colored
        );
        assert!(colored.contains("\x1b[1;31m^^^^^\x1b[0m"), "{}", colored);
        assert!(
            colored.contains("\x1b[1;33mwarning[W0001]:\x1b[0m"),
            "{}",
            colored
        );
        assert!(
            colored.contains("\x1b[1;31merror: could not compile due to 1 previous error\x1b[0m"),
            "{}",
            colored
        );
        assert!(
            colored.contains("\x1b[1;33mwarning: 1 warning generated\x1b[0m"),
            "{}",
            colored
        );
        // Forced off: no escapes.
        let off = ds.render_all_with(&db, false);
        assert!(!off.contains("\x1b["), "{}", off);
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
    fn render_all_ends_with_newline() {
        let db = SourceDb::new();
        let mut ds = Diagnostics::new();
        ds.push(Diagnostic::error("E0001", None, "e1"));
        let out = ds.render_all_with(&db, false);
        assert!(
            out.ends_with("error: could not compile due to 1 previous error\n"),
            "{}",
            out
        );
        // Empty diagnostics still render as an empty string.
        assert_eq!(Diagnostics::new().render_all_with(&db, false), "");
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

    #[test]
    fn every_known_code_has_an_explanation() {
        for c in KNOWN_CODES {
            assert!(explain_code(c).is_some(), "no explanation for {}", c);
        }
        // Lint/analyze codes live outside KNOWN_CODES, but are explainable too.
        for c in [
            "W0002", "W0003", "W0101", "W0102", "W0201", "W0501", "W0502",
        ] {
            assert!(explain_code(c).is_some(), "no explanation for {}", c);
        }
        assert!(explain_code("E0301").unwrap().contains("expected"));
        assert!(explain_code("X9999").is_none());
        assert_eq!(explained_codes().count(), CODE_EXPLAIN.len());
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;
    use crate::source::{FileId, Span};

    #[test]
    fn diags_encode_decode_roundtrip() {
        let ds = vec![
            Diagnostic::error("E0301", Some(Span::new(FileId(0), 3, 9)), "expected `;`")
                .with_help("add it"),
            Diagnostic::warning("W0001", None, "file-level warning"),
            Diagnostic {
                code: "E9999", // unknown code collapses to E0000, no panic/leak
                severity: Severity::Note,
                span: None,
                message: "note".into(),
                help: None,
            },
        ];
        let bytes = encode_diags(&ds);
        let back = decode_diags(FileId(0), &bytes).unwrap();
        assert_eq!(back.len(), 3);
        assert_eq!(back[0].code, "E0301");
        assert_eq!(back[0].severity, Severity::Error);
        assert_eq!(back[0].span, Some(Span::new(FileId(0), 3, 9)));
        assert_eq!(back[0].message, "expected `;`");
        assert_eq!(back[0].help.as_deref(), Some("add it"));
        assert_eq!(back[1].span, None);
        assert_eq!(back[2].code, "E0000");
        assert!(decode_diags(FileId(0), &bytes[..3]).is_none());
    }
}
