//! `hdlsim fmt` — format Verilog/VHDL sources with the canonical formatter in
//! `hdl_syntax::fmt` (token-stream based: keeps the author's line structure,
//! re-derives indentation, normalizes horizontal spacing; idempotent).
//!
//! Modes: default prints the formatted file to stdout (single file only);
//! `--check` lists files that would change and exits 1 (CI gate); `-w` writes
//! in place.

use super::{parse_with, Command};
use clap::Parser;
use hdl_syntax::diagnostic::Severity;
use hdl_syntax::language::Language;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    about = "format sources; --check exits 1 when a file needs formatting: hdlsim fmt -w [files]"
)]
struct FmtArgs {
    /// List files that would change and exit 1 (CI gate).
    #[arg(long)]
    check: bool,
    /// Rewrite files in place.
    #[arg(long, short = 'w')]
    write: bool,
    /// Files to format.
    files: Vec<PathBuf>,
}

pub struct Fmt;

impl Command for Fmt {
    fn name(&self) -> &'static str {
        "fmt"
    }
    fn about(&self) -> &'static str {
        "format sources; --check exits 1 when a file needs formatting: hdlsim fmt -w [files]"
    }
    fn run(&self, args: &[String]) -> anyhow::Result<i32> {
        let a = match parse_with::<FmtArgs>("fmt", args) {
            Ok(a) => a,
            Err(code) => return Ok(code),
        };
        let check = a.check;
        let write = a.write;
        if a.files.is_empty() {
            return Err(anyhow::anyhow!(
                "usage: hdlsim fmt [--check] [-w] <file.v|file.vhd> ..."
            ));
        }
        if check && write {
            return Err(anyhow::anyhow!("--check and -w are mutually exclusive"));
        }
        if !check && !write && a.files.len() > 1 {
            return Err(anyhow::anyhow!(
                "multiple files need --check or -w (default prints one file to stdout)"
            ));
        }
        let mut unformatted = 0;
        for f in &a.files {
            let text = std::fs::read_to_string(f)
                .map_err(|e| anyhow::anyhow!("cannot read {}: {}", f.display(), e))?;
            let lang = Language::from_path(f)
                .ok_or_else(|| format!("unknown file extension: {}", f.display()))
                .map_err(anyhow::Error::msg)?;
            let (out, diags) = hdl_syntax::fmt::format_source(lang, &text);
            let errs = diags
                .iter()
                .filter(|d| d.severity == Severity::Error)
                .count();
            if errs > 0 {
                eprintln!(
                    "hdlsim fmt: {}: {} lex error(s) (formatted anyway)",
                    f.display(),
                    errs
                );
            }
            if check {
                if out != text {
                    println!("{}", f.display());
                    unformatted += 1;
                }
            } else if write {
                if out != text {
                    std::fs::write(f, &out)
                        .map_err(|e| anyhow::anyhow!("cannot write {}: {}", f.display(), e))?;
                    println!("reformatted {}", f.display());
                }
            } else {
                print!("{}", out);
            }
        }
        Ok(if check && unformatted > 0 { 1 } else { 0 })
    }
}
