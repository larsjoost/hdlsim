//! `hdlsim project` — generate project files for external tools (Vivado TCL,
//! JSON manifest) from sources + library mappings. Thin wrapper over
//! [`crate::project::generate`].

use super::{build_manager, parse_with, remap_into_bundle, Command, CommonArgs};
use crate::project::{generate, ProjectFormat, ProjectSpec};
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    about = "generate a project file for another tool: hdlsim project --format vivado|json [--out f.tcl] [--top t] [--sim-top tb] [files]"
)]
struct ProjectArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// Output format (vivado-tcl|json).
    #[arg(long, short = 'f')]
    format: Option<String>,
    /// Output file (default: stdout).
    #[arg(long, short = 'o')]
    out: Option<PathBuf>,
    #[arg(long, short = 'n')]
    name: Option<String>,
    #[arg(long)]
    top: Option<String>,
    #[arg(long)]
    sim_top: Option<String>,
    #[arg(long)]
    part: Option<String>,
    /// Source files.
    files: Vec<PathBuf>,
}

pub struct Project;

impl Command for Project {
    fn name(&self) -> &'static str {
        "project"
    }
    fn about(&self) -> &'static str {
        "generate a project file for another tool: hdlsim project --format vivado|json [--out f.tcl] [--top t] [--sim-top tb] [files]"
    }
    fn run(&self, args: &[String]) -> Result<i32, String> {
        let a = match parse_with::<ProjectArgs>("project", args) {
            Ok(a) => a,
            Err(code) => return Ok(code),
        };
        let opts = a.common.into_opts()?;
        let format = ProjectFormat::from_str(
            a.format
                .as_deref()
                .ok_or("missing --format (vivado|json)")?,
        )?;
        let out = a.out;
        let mut spec = ProjectSpec::default();
        spec.name = a.name.unwrap_or_default();
        spec.top = a.top;
        spec.sim_top = a.sim_top;
        spec.part = a.part;
        spec.files = a.files;
        if spec.files.is_empty() {
            return Err("no input files".to_string());
        }
        let (mgr, root) = build_manager(&opts)?;
        spec.files = spec
            .files
            .iter()
            .map(|f| remap_into_bundle(f, &root))
            .collect();
        spec.libraries = opts
            .libs
            .iter()
            .map(|(n, p)| (n.clone(), p.clone()))
            .collect();
        spec.include_dirs = opts.incdirs.clone();
        if spec.name.is_empty() {
            spec.name = "hdlsim_proj".to_string();
        }
        let text = generate(&spec, format, Some(&mgr))?;
        match out {
            Some(path) => {
                std::fs::write(&path, &text)
                    .map_err(|e| format!("cannot write {}: {}", path.display(), e))?;
                println!("hdlsim: wrote {} ({})", path.display(), format.extension());
            }
            None => print!("{}", text),
        }
        Ok(0)
    }
}
