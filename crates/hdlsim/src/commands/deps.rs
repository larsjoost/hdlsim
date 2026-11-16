//! `hdlsim deps` — list every source file a top-level unit depends on,
//! transitively. Compiles the given files, elaborates the top (which resolves
//! missing units on demand from the libraries), then prints the compiled
//! file set — one canonical path per line, sorted.

use super::{build_manager, parse_with, remap_into_bundle, Command, CommonArgs};
use clap::Parser;
use hdl_ir::elab;
use hdl_ir::Design;
use std::path::PathBuf;

#[derive(Parser)]
#[command(about = "list all files a top unit depends on: hdlsim deps --top tb [files...]")]
struct DepsArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// Top-level unit to elaborate.
    #[arg(long)]
    top: String,
    /// Source files.
    files: Vec<PathBuf>,
}

pub struct Deps;

impl Command for Deps {
    fn name(&self) -> &'static str {
        "deps"
    }
    fn about(&self) -> &'static str {
        "list all files a top unit depends on: hdlsim deps --top tb [files...]"
    }
    fn run(&self, args: &[String]) -> Result<i32, String> {
        let a = match parse_with::<DepsArgs>("deps", args) {
            Ok(a) => a,
            Err(code) => return Ok(code),
        };
        let opts = a.common.into_opts()?;
        let (mut mgr, root) = build_manager(&opts)?;
        let files: Vec<PathBuf> = a
            .files
            .iter()
            .map(|f| remap_into_bundle(f, &root))
            .collect();
        let mut design = Design::default();
        mgr.compile_files(&mut design, &files);
        // Elaborating the top pulls in every transitively referenced unit.
        let res = elab::elaborate_with(&mut design, &a.top, &mut mgr);
        let mut failed = false;
        if let Err(e) = res {
            eprintln!("hdlsim deps: elaboration error: {}", e);
            failed = true;
        }
        if mgr.diags().has_errors() {
            eprint!("{}", mgr.diags().render_all(mgr.db()));
            failed = true;
        }
        if failed {
            return Ok(1);
        }
        for f in mgr.compiled_files() {
            println!("{}", f.display());
        }
        Ok(0)
    }
}
