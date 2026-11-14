//! `hdlsim fmt` — format Verilog/VHDL sources with the canonical formatter in
//! `hdl_syntax::fmt` (token-stream based: keeps the author's line structure,
//! re-derives indentation, normalizes horizontal spacing; idempotent).
//!
//! Modes: default prints the formatted file to stdout (single file only);
//! `--check` lists files that would change and exits 1 (CI gate); `-w` writes
//! in place.

use super::Command;
use hdl_syntax::diagnostic::Severity;
use hdl_syntax::language::Language;
use std::path::PathBuf;

pub struct Fmt;

impl Command for Fmt {
    fn name(&self) -> &'static str {
        "fmt"
    }
    fn about(&self) -> &'static str {
        "format sources: hdlsim fmt [--check] [-w] file.v ... (default: print to stdout)"
    }
    fn run(&self, args: &[String]) -> Result<i32, String> {
        let mut check = false;
        let mut write = false;
        let mut files: Vec<PathBuf> = Vec::new();
        for a in args {
            match a.as_str() {
                "--check" => check = true,
                "-w" | "--write" => write = true,
                other if other.starts_with('-') => {
                    return Err(format!("unknown flag '{}'", other));
                }
                f => files.push(PathBuf::from(f)),
            }
        }
        if files.is_empty() {
            return Err("usage: hdlsim fmt [--check] [-w] <file.v|file.vhd> ...".to_string());
        }
        if check && write {
            return Err("--check and -w are mutually exclusive".to_string());
        }
        if !check && !write && files.len() > 1 {
            return Err(
                "multiple files need --check or -w (default prints one file to stdout)".to_string(),
            );
        }
        let mut unformatted = 0;
        for f in &files {
            let text = std::fs::read_to_string(f)
                .map_err(|e| format!("cannot read {}: {}", f.display(), e))?;
            let lang = Language::from_path(f)
                .ok_or_else(|| format!("unknown file extension: {}", f.display()))?;
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
                        .map_err(|e| format!("cannot write {}: {}", f.display(), e))?;
                    println!("reformatted {}", f.display());
                }
            } else {
                print!("{}", out);
            }
        }
        Ok(if check && unformatted > 0 { 1 } else { 0 })
    }
}
