//! `hdlsim lint` — run the source linter (`hdl_syntax::lint`) over files.
//!
//! Default: print rustc-style diagnostics for every finding, exit 1 if any.
//! `--fix`: apply the safe automatic fixes (trailing whitespace, tabs) in
//! place, then report whatever remains.

use super::{parse_with, Command};
use clap::Parser;
use hdl_syntax::diagnostic::Diagnostic;
use hdl_syntax::language::Language;
use hdl_syntax::lint::{apply_fixes, lint_source, Lint};
use hdl_syntax::source::SourceDb;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    about = "flag common mistakes and style issues; --fix applies the safe ones: hdlsim lint [files]"
)]
struct LintArgs {
    /// Apply safe automatic fixes in place.
    #[arg(long)]
    fix: bool,
    /// Files to lint.
    files: Vec<PathBuf>,
}

pub struct LintCmd;

impl Command for LintCmd {
    fn name(&self) -> &'static str {
        "lint"
    }
    fn about(&self) -> &'static str {
        "flag common mistakes and style issues; --fix applies the safe ones: hdlsim lint [files]"
    }
    fn run(&self, args: &[String]) -> anyhow::Result<i32> {
        let a = match parse_with::<LintArgs>("lint", args) {
            Ok(a) => a,
            Err(code) => return Ok(code),
        };
        let fix = a.fix;
        if a.files.is_empty() {
            return Err(anyhow::anyhow!(
                "usage: hdlsim lint [--fix] <file.v|file.vhd> ..."
            ));
        }
        let mut total = 0;
        let mut files_with_issues = 0;
        for f in &a.files {
            let text = std::fs::read_to_string(f)
                .map_err(|e| anyhow::anyhow!("cannot read {}: {}", f.display(), e))?;
            let lang = Language::from_path(f)
                .ok_or_else(|| format!("unknown file extension: {}", f.display()))
                .map_err(anyhow::Error::msg)?;
            let name = f.display().to_string();
            let (lints, db) = if fix {
                let mut db = SourceDb::new();
                let lints = lint_source(lang, &mut db, &name, &text);
                let (fixed, applied) = apply_fixes(&text, &lints);
                if applied > 0 {
                    std::fs::write(f, &fixed)
                        .map_err(|e| anyhow::anyhow!("cannot write {}: {}", f.display(), e))?;
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
