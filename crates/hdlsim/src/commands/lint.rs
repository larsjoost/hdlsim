//! `hdlsim lint` — run the source linter (`hdl_syntax::lint`) over files.
//!
//! Default: print rustc-style diagnostics for every finding, exit 1 if any.
//! `--fix`: apply the safe automatic fixes (trailing whitespace, tabs) in
//! place, then report whatever remains.

use super::Command;
use hdl_syntax::diagnostic::Diagnostic;
use hdl_syntax::language::Language;
use hdl_syntax::lint::{apply_fixes, lint_source, Lint};
use hdl_syntax::source::SourceDb;
use std::path::PathBuf;

pub struct LintCmd;

impl Command for LintCmd {
    fn name(&self) -> &'static str {
        "lint"
    }
    fn about(&self) -> &'static str {
        "lint sources: hdlsim lint [--fix] file.v ... (exit 1 if any finding)"
    }
    fn run(&self, args: &[String]) -> Result<i32, String> {
        let mut fix = false;
        let mut files: Vec<PathBuf> = Vec::new();
        for a in args {
            match a.as_str() {
                "--fix" => fix = true,
                other if other.starts_with('-') => {
                    return Err(format!("unknown flag '{}'", other));
                }
                f => files.push(PathBuf::from(f)),
            }
        }
        if files.is_empty() {
            return Err("usage: hdlsim lint [--fix] <file.v|file.vhd> ...".to_string());
        }
        let mut total = 0;
        let mut files_with_issues = 0;
        for f in &files {
            let text = std::fs::read_to_string(f)
                .map_err(|e| format!("cannot read {}: {}", f.display(), e))?;
            let lang = Language::from_path(f)
                .ok_or_else(|| format!("unknown file extension: {}", f.display()))?;
            let name = f.display().to_string();
            let (lints, db) = if fix {
                let mut db = SourceDb::new();
                let lints = lint_source(lang, &mut db, &name, &text);
                let (fixed, applied) = apply_fixes(&text, &lints);
                if applied > 0 {
                    std::fs::write(f, &fixed)
                        .map_err(|e| format!("cannot write {}: {}", f.display(), e))?;
                    eprintln!("hdlsim lint: {}: applied {} fix(es)", f.display(), applied);
                }
                // Re-lint the fixed text so remaining spans are valid.
                let mut db2 = SourceDb::new();
                let remaining = lint_source(lang, &mut db2, &name, &fixed);
                (remaining, db2)
            } else {
                let mut db = SourceDb::new();
                let lints = lint_source(lang, &mut db, &name, &text);
                (lints, db)
            };
            if !lints.is_empty() {
                files_with_issues += 1;
            }
            total += lints.len();
            for l in &lints {
                eprint!("{}", render(l, &db));
            }
        }
        println!(
            "hdlsim lint: {} finding(s) in {} file(s)",
            total, files_with_issues
        );
        Ok(if total > 0 { 1 } else { 0 })
    }
}

fn render(l: &Lint, db: &SourceDb) -> String {
    Diagnostic {
        code: l.code,
        severity: l.severity,
        span: Some(l.span),
        message: l.message.clone(),
        help: l
            .fix
            .as_ref()
            .map(|_| "automatic fix available (--fix)".to_string()),
    }
    .render(db)
}
