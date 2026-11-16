//! `hdlsim ast` — parse a file and dump its concrete syntax tree (S-expression).
//!
//! This is the "syntax tree as a reusable artifact" surface: other tools can
//! consume the same CST (see `hdl-syntax`) that the simulator lowers from.

use super::{parse_with, Command};
use clap::Parser;
use hdl_syntax::language::Language;
use hdl_syntax::source::SourceDb;
use hdl_syntax::verilog::{parser, preproc};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Parser)]
#[command(about = "parse a file and print its CST (s-expression) — the reusable syntax tree")]
struct AstArgs {
    /// Source file to parse.
    file: PathBuf,
}

pub struct Ast;

impl Command for Ast {
    fn name(&self) -> &'static str {
        "ast"
    }
    fn about(&self) -> &'static str {
        "parse a file and print its CST (s-expression) — the reusable syntax tree"
    }
    fn run(&self, args: &[String]) -> anyhow::Result<i32> {
        let a = match parse_with::<AstArgs>("ast", args) {
            Ok(a) => a,
            Err(code) => return Ok(code),
        };
        let path = &a.file;
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("cannot read {}: {}", path.display(), e))?;
        let lang = Language::from_path(path)
            .ok_or_else(|| format!("unknown file extension: {}", path.display()))
            .map_err(anyhow::Error::msg)?;
        match lang {
            Language::Verilog => {
                let mut db = SourceDb::new();
                let raw_id = db
                    .add_file(path.display().to_string(), text)
                    .map_err(anyhow::Error::msg)?;
                let mut macros = HashMap::new();
                let mut pdiags = Vec::new();
                let expanded =
                    preproc::preprocess(raw_id, db.file(raw_id), &[], &mut macros, &mut pdiags);
                let id = db
                    .add_file(path.display().to_string(), expanded)
                    .map_err(anyhow::Error::msg)?;
                let parsed = parser::parse_file(id, db.file(id));
                for d in pdiags.iter().chain(parsed.diags.iter()) {
                    eprint!("{}", d.render(&db));
                }
                println!("{}", parsed.root.debug_sexp(&db));
                if parsed
                    .diags
                    .iter()
                    .any(|d| d.severity == hdl_syntax::diagnostic::Severity::Error)
                {
                    Ok(1)
                } else {
                    Ok(0)
                }
            }
            Language::Vhdl => {
                let mut db = SourceDb::new();
                let id = db
                    .add_file(path.display().to_string(), text)
                    .map_err(anyhow::Error::msg)?;
                let parsed = hdl_syntax::vhdl::parser::parse_file(id, db.file(id));
                for d in &parsed.diags {
                    eprint!("{}", d.render(&db));
                }
                println!("{}", parsed.root.debug_sexp(&db));
                if parsed
                    .diags
                    .iter()
                    .any(|d| d.severity == hdl_syntax::diagnostic::Severity::Error)
                {
                    Ok(1)
                } else {
                    Ok(0)
                }
            }
        }
    }
}
