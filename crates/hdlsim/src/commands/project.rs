//! `hdlsim project` — generate project files for external tools (Vivado TCL,
//! JSON manifest) from sources + library mappings. Thin wrapper over
//! [`crate::project::generate`].

use super::{build_manager, parse_with, remap_into_bundle, Command, CommonArgs};
use crate::project::{generate, ProjectFormat, ProjectSpec};
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    about = "export the file list for Vivado (Tcl) or as a JSON manifest: hdlsim project --format vivado [--top t]"
)]
struct ProjectArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// Output format (vivado-tcl|json).
    /// Project file format: vivado (Tcl script) or json (file/unit manifest).
    #[arg(long, short = 'f')]
    format: Option<String>,
    /// Output file (default: stdout).
    #[arg(long, short = 'o')]
    out: Option<PathBuf>,
    /// Design name recorded in the project file.
    #[arg(long, short = 'n')]
    name: Option<String>,
    /// FPGA top entity recorded in the project file.
    #[arg(long)]
    top: Option<String>,
    /// Testbench top recorded as the simulation top.
    #[arg(long)]
    sim_top: Option<String>,
    /// FPGA part recorded in the project file (e.g. xc7a35tcpg236-1).
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
        "export the file list for Vivado (Tcl) or as a JSON manifest: hdlsim project --format vivado [--top t]"
    }
    fn run(&self, args: &[String]) -> anyhow::Result<i32> {
        let a = match parse_with::<ProjectArgs>("project", args) {
            Ok(a) => a,
            Err(code) => return Ok(code),
        };
        let opts = a.common.into_opts()?;
        let format = ProjectFormat::parse(
            a.format
                .as_deref()
                .ok_or("missing --format (vivado|json)")
                .map_err(anyhow::Error::msg)?,
        )
        .map_err(anyhow::Error::msg)?;
        let out = a.out;
        let mut spec = ProjectSpec {
            name: a.name.unwrap_or_default(),
            top: a.top,
            sim_top: a.sim_top,
            part: a.part,
            files: a.files,
            ..Default::default()
        };
        if spec.files.is_empty() {
            return Err(anyhow::anyhow!("no input files"));
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
        let text = generate(&spec, format, Some(&mgr)).map_err(anyhow::Error::msg)?;
        match out {
            Some(path) => {
                std::fs::write(&path, &text)
                    .map_err(|e| anyhow::anyhow!("cannot write {}: {}", path.display(), e))?;
                println!("hdlsim: wrote {} ({})", path.display(), format.extension());
            }
            None => print!("{}", text),
        }
        Ok(0)
    }
}
