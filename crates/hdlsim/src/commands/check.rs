//! `hdlsim check` — compile only (parse + lower + resolve), print diagnostics,
//! no simulation. Like `vlog -lint` / `ghdl -a`.

use super::{build_manager, parse_with, remap_into_bundle, Command, CommonArgs};
use anyhow::Context as _;
use clap::Parser;
use hdl_ir::elab;
use hdl_ir::Design;
use hdl_syntax::diagnostic::Severity;
use hdl_syntax::source::SourceDb;
use std::path::PathBuf;

#[derive(Parser)]
#[command(about = "compile files and report diagnostics without simulating")]
struct CheckArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// Parse only: report lexical/syntax errors for each file on its own,
    /// without compiling dependencies or resolving libraries. Fast check
    /// for a file you just edited.
    #[arg(long)]
    syntax_only: bool,
    /// Source files to compile.
    files: Vec<PathBuf>,
}

pub struct Check;

/// Parse each file standalone and report lexical/syntax diagnostics. No
/// libraries, no dependency compilation: a file that references units it
/// does not define still passes if its syntax is valid.
fn syntax_only(files: &[PathBuf]) -> anyhow::Result<i32> {
    let mut db = SourceDb::new();
    let mut all_diags = hdl_syntax::diagnostic::Diagnostics::new();
    for f in files {
        let text =
            std::fs::read_to_string(f).with_context(|| format!("cannot read {}", f.display()))?;
        let id = db
            .add_file(f.display().to_string(), text)
            .map_err(anyhow::Error::msg)?;
        let ext = f
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase());
        match ext.as_deref() {
            Some("v") | Some("sv") => {
                all_diags.extend(hdl_syntax::verilog::parser::parse_file(id, db.file(id)).diags);
            }
            _ => {
                all_diags.extend(hdl_syntax::vhdl::parser::parse_file(id, db.file(id)).diags);
            }
        }
    }
    for d in &all_diags.items {
        eprint!("{}", d.render(&db));
    }
    let errors = all_diags
        .items
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .count();
    if all_diags.has_errors() {
        println!("hdlsim check --syntax-only: {errors} error(s)");
        Ok(1)
    } else if all_diags.is_empty() {
        println!("hdlsim check --syntax-only: ok ({} file(s))", files.len());
        Ok(0)
    } else {
        println!(
            "hdlsim check --syntax-only: ok ({} warning(s))",
            all_diags.items.len()
        );
        Ok(0)
    }
}

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
        if a.syntax_only {
            return syntax_only(&a.files);
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
