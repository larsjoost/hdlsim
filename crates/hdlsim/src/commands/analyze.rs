//! `hdlsim analyze` — static design heuristics over the compiled IR
//! (currently: never-driven W0501 / never-read W0502 signals per module).
//! Prints lint-style findings and exits 1 if any are found, like `lint`.

use super::{build_manager, extract_common, remap_into_bundle, Command};
use hdl_ir::analyze;
use hdl_ir::Design;
use hdl_syntax::diagnostic::explain_code;
use std::path::PathBuf;

pub struct Analyze;

impl Command for Analyze {
    fn name(&self) -> &'static str {
        "analyze"
    }
    fn about(&self) -> &'static str {
        "static design heuristics (undriven/unread signals): hdlsim analyze [files...]"
    }
    fn run(&self, args: &[String]) -> Result<i32, String> {
        let (opts, rest) = extract_common(args)?;
        let files: Vec<PathBuf> = rest.iter().map(PathBuf::from).collect();
        if files.is_empty() {
            return Err("no input files".to_string());
        }
        let (mut mgr, root) = build_manager(&opts)?;
        let files: Vec<PathBuf> = files.iter().map(|f| remap_into_bundle(f, &root)).collect();
        let mut design = Design::default();
        mgr.compile_files(&mut design, &files);
        if mgr.diags().has_errors() {
            eprint!("{}", mgr.diags().render_all(mgr.db()));
            return Ok(1);
        }
        let findings = analyze::analyze_design(&design);
        for f in &findings {
            eprintln!("{}", f.render());
            if let Some(ex) = explain_code(f.code) {
                eprintln!("   = note[{}]: {}", f.code, ex);
            }
        }
        if findings.is_empty() {
            println!(
                "hdlsim analyze: ok ({} modules, no findings)",
                design.modules.len()
            );
            Ok(0)
        } else {
            println!("hdlsim analyze: {} finding(s)", findings.len());
            Ok(1)
        }
    }
}
