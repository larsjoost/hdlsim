//! `hdlsim check` — compile only (parse + lower + resolve), print diagnostics,
//! no simulation. Like `vlog -lint` / `ghdl -a`.

use super::{build_manager, parse_with, remap_into_bundle, Command, CommonArgs};
use clap::Parser;
use hdl_ir::elab;
use hdl_ir::Design;
use std::path::PathBuf;

#[derive(Parser)]
#[command(about = "compile files and report diagnostics without simulating")]
struct CheckArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// Source files to compile.
    files: Vec<PathBuf>,
}

pub struct Check;

impl Command for Check {
    fn name(&self) -> &'static str {
        "check"
    }
    fn about(&self) -> &'static str {
        "compile files and report diagnostics without simulating"
    }
    fn run(&self, args: &[String]) -> anyhow::Result<i32> {
        let a = match parse_with::<CheckArgs>("check", args) {
            Ok(a) => a,
            Err(code) => return Ok(code),
        };
        let opts = a.common.into_opts()?;
        if a.files.is_empty() {
            return Err(anyhow::anyhow!("no input files"));
        }
        let (mut mgr, root) = build_manager(&opts)?;
        let files: Vec<PathBuf> = a
            .files
            .iter()
            .map(|f| remap_into_bundle(f, &root))
            .collect();
        let mut design = Design::default();
        mgr.compile_files(&mut design, &files);
        // Resolve every compiled top-level candidate: elaborate each module
        // that nothing instantiates, so missing dependencies are reported too.
        let tops: Vec<String> = design.modules.iter().map(|m| m.name.clone()).collect();
        for t in &tops {
            // No simulation follows, so package function bodies need not be
            // copied into each elaboration (O(all bodies) per top otherwise).
            let _ = elab::elaborate_with_opts(&mut design, t, &mut mgr, false);
        }
        let diags = mgr.diags();
        if !diags.is_empty() {
            eprint!("{}", diags.render_all(mgr.db()));
        }
        if diags.has_errors() {
            println!("hdlsim check: {} error(s)", diags.items.len());
            Ok(1)
        } else {
            println!(
                "hdlsim check: ok ({} modules, {} diagnostics)",
                design.modules.len(),
                diags.items.len()
            );
            Ok(0)
        }
    }
}
