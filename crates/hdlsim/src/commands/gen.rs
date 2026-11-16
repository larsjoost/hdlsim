//! `hdlsim gen tb` — generate a testbench skeleton for a top-level unit from
//! the elaborated IR (ports become DUT signals + port map, plus a clock
//! generator and a stimulus shell). The language is inferred from the file
//! that defines the top (`--lang verilog|vhdl` overrides). Output goes to
//! stdout, or to `-o FILE`.

use super::{build_manager, parse_with, remap_into_bundle, Command, CommonArgs};
use clap::Parser;
use hdl_ir::elab::UnitResolver;
use hdl_ir::gen;
use hdl_ir::Design;
use hdl_syntax::language::Language;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    about = "generate code from the design: hdlsim gen tb --top m [-o out] [--lang verilog|vhdl] [files...]"
)]
struct GenArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// What to generate ("tb").
    what: String,
    /// Top-level unit.
    #[arg(long)]
    top: Option<String>,
    /// Output file (default: stdout).
    #[arg(long, short = 'o')]
    out: Option<PathBuf>,
    /// Output language override (verilog|vhdl).
    #[arg(long)]
    lang: Option<String>,
    /// Source files.
    files: Vec<PathBuf>,
}

pub struct Gen;

impl Command for Gen {
    fn name(&self) -> &'static str {
        "gen"
    }
    fn about(&self) -> &'static str {
        "generate code from the design: hdlsim gen tb --top m [-o out] [--lang verilog|vhdl] [files...]"
    }
    fn run(&self, args: &[String]) -> Result<i32, String> {
        let a = match parse_with::<GenArgs>("gen", args) {
            Ok(a) => a,
            Err(code) => return Ok(code),
        };
        let opts = a.common.into_opts()?;
        if a.what != "tb" {
            return Err(
                "usage: hdlsim gen tb --top <unit> [-o out.v] [--lang verilog|vhdl] [files...]"
                    .to_string(),
            );
        }
        let lang = match a.lang.as_deref().map(str::to_ascii_lowercase) {
            Some(v) => match v.as_str() {
                "verilog" | "v" | "sv" => Some(Language::Verilog),
                "vhdl" | "vhd" => Some(Language::Vhdl),
                other => return Err(format!("unknown language '{}'", other)),
            },
            None => None,
        };
        let top = a.top.ok_or("--top is required")?;
        let out = a.out;
        let files: Vec<PathBuf> = a.files;
        let (mut mgr, root) = build_manager(&opts)?;
        let files: Vec<PathBuf> = files.iter().map(|f| remap_into_bundle(f, &root)).collect();
        let mut design = Design::default();
        if !files.is_empty() {
            mgr.compile_files(&mut design, &files);
        }
        // Resolve the top from the libraries if it was not among the given
        // files. (No full elaboration: a DUT with ports is not a valid sim
        // top, and we only need its ModuleIr.)
        if design.modules.iter().all(|m| m.name != top) {
            if let Err(e) = mgr.resolve(&mut design, &top) {
                eprintln!("hdlsim gen: {}", e);
                return Ok(1);
            }
        }
        if design.modules.iter().all(|m| m.name != top) {
            eprintln!("hdlsim gen: no module named '{}'", top);
            return Ok(1);
        }
        if mgr.diags().has_errors() {
            eprint!("{}", mgr.diags().render_all(mgr.db()));
            return Ok(1);
        }
        let lang = match lang {
            Some(l) => l,
            None => mgr
                .unit_file(&top)
                .and_then(|p| Language::from_path(&p))
                .or_else(|| files.first().and_then(|f| Language::from_path(f)))
                .unwrap_or(Language::Verilog),
        };
        let src = gen::testbench(&design, &top, lang)?;
        match &out {
            Some(p) => {
                std::fs::write(p, &src)
                    .map_err(|e| format!("cannot write {}: {}", p.display(), e))?;
                println!(
                    "wrote {} ({} lines, {})",
                    p.display(),
                    src.lines().count(),
                    lang.name()
                );
            }
            None => print!("{}", src),
        }
        Ok(0)
    }
}
