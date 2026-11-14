//! `hdlsim check` — compile only (parse + lower + resolve), print diagnostics,
//! no simulation. Like `vlog -lint` / `ghdl -a`.

use super::{build_manager, extract_common, Command};
use hdl_ir::elab;
use hdl_ir::Design;
use std::path::PathBuf;

pub struct Check;

impl Command for Check {
    fn name(&self) -> &'static str {
        "check"
    }
    fn about(&self) -> &'static str {
        "compile files and report diagnostics without simulating"
    }
    fn run(&self, args: &[String]) -> Result<i32, String> {
        let (opts, rest) = extract_common(args)?;
        let files: Vec<PathBuf> = rest.iter().map(PathBuf::from).collect();
        if files.is_empty() {
            return Err("no input files".to_string());
        }
        let mut mgr = build_manager(&opts)?;
        let mut design = Design::default();
        mgr.compile_files(&mut design, &files);
        // Resolve every compiled top-level candidate: elaborate each module
        // that nothing instantiates, so missing dependencies are reported too.
        let tops: Vec<String> = design.modules.iter().map(|m| m.name.clone()).collect();
        for t in &tops {
            let _ = elab::elaborate_with(&mut design, t, &mut mgr);
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
