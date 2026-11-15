//! `hdlsim gen tb` — generate a testbench skeleton for a top-level unit from
//! the elaborated IR (ports become DUT signals + port map, plus a clock
//! generator and a stimulus shell). The language is inferred from the file
//! that defines the top (`--lang verilog|vhdl` overrides). Output goes to
//! stdout, or to `-o FILE`.

use super::{build_manager, extract_common, remap_into_bundle, Command};
use hdl_ir::elab::UnitResolver;
use hdl_ir::gen;
use hdl_ir::Design;
use hdl_syntax::language::Language;
use std::path::PathBuf;

pub struct Gen;

impl Command for Gen {
    fn name(&self) -> &'static str {
        "gen"
    }
    fn about(&self) -> &'static str {
        "generate code from the design: hdlsim gen tb --top m [-o out] [--lang verilog|vhdl] [files...]"
    }
    fn run(&self, args: &[String]) -> Result<i32, String> {
        let (opts, rest) = extract_common(args)?;
        if rest.first().map(|s| s.as_str()) != Some("tb") {
            return Err(
                "usage: hdlsim gen tb --top <unit> [-o out.v] [--lang verilog|vhdl] [files...]"
                    .to_string(),
            );
        }
        let rest = &rest[1..];
        let mut top: Option<String> = None;
        let mut out: Option<PathBuf> = None;
        let mut lang: Option<Language> = None;
        let mut files: Vec<PathBuf> = Vec::new();
        let mut i = 0;
        while i < rest.len() {
            match rest[i].as_str() {
                "--top" => {
                    top = Some(rest.get(i + 1).ok_or("--top needs a value")?.clone());
                    i += 2;
                }
                "-o" | "--out" => {
                    out = Some(PathBuf::from(rest.get(i + 1).ok_or("-o needs a value")?));
                    i += 2;
                }
                "--lang" => {
                    let v = rest.get(i + 1).ok_or("--lang needs a value")?;
                    lang = Some(match v.to_ascii_lowercase().as_str() {
                        "verilog" | "v" | "sv" => Language::Verilog,
                        "vhdl" | "vhd" => Language::Vhdl,
                        other => return Err(format!("unknown language '{}'", other)),
                    });
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
        let top = top.ok_or("--top is required")?;
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
