//! `hdlsim deps` — list every source file a top-level unit depends on,
//! transitively. Compiles the given files, elaborates the top (which resolves
//! missing units on demand from the libraries), then prints the compiled
//! file set — one canonical path per line, sorted.

use super::{build_manager, extract_common, remap_into_bundle, Command};
use hdl_ir::elab;
use hdl_ir::Design;
use std::path::PathBuf;

pub struct Deps;

impl Command for Deps {
    fn name(&self) -> &'static str {
        "deps"
    }
    fn about(&self) -> &'static str {
        "list all files a top unit depends on: hdlsim deps --top tb [files...]"
    }
    fn run(&self, args: &[String]) -> Result<i32, String> {
        let (opts, rest) = extract_common(args)?;
        let mut top: Option<String> = None;
        let mut files: Vec<PathBuf> = Vec::new();
        let mut i = 0;
        while i < rest.len() {
            match rest[i].as_str() {
                "--top" => {
                    top = Some(rest.get(i + 1).ok_or("--top needs a value")?.clone());
                    i += 2;
                }
                other if other.starts_with("--") => {
                    return Err(format!("unknown flag '{}'", other));
                }
                f => {
                    files.push(PathBuf::from(f));
                    i += 1;
                }
            }
        }
        let top = top.ok_or("usage: hdlsim deps --top <unit> [files...]")?;
        let (mut mgr, root) = build_manager(&opts)?;
        let files: Vec<PathBuf> = files.iter().map(|f| remap_into_bundle(f, &root)).collect();
        let mut design = Design::default();
        mgr.compile_files(&mut design, &files);
        // Elaborating the top pulls in every transitively referenced unit.
        let res = elab::elaborate_with(&mut design, &top, &mut mgr);
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
