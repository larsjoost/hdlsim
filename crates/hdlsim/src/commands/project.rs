//! `hdlsim project` — generate project files for external tools (Vivado TCL,
//! JSON manifest) from sources + library mappings. Thin wrapper over
//! [`crate::project::generate`].

use super::{build_manager, extract_common, Command};
use crate::project::{generate, ProjectFormat, ProjectSpec};
use std::path::PathBuf;

pub struct Project;

impl Command for Project {
    fn name(&self) -> &'static str {
        "project"
    }
    fn about(&self) -> &'static str {
        "generate a project file for another tool: hdlsim project --format vivado|json [--out f.tcl] [--top t] [--sim-top tb] [files]"
    }
    fn run(&self, args: &[String]) -> Result<i32, String> {
        let (opts, rest) = extract_common(args)?;
        let mut format: Option<ProjectFormat> = None;
        let mut out: Option<PathBuf> = None;
        let mut spec = ProjectSpec::default();
        let mut i = 0;
        while i < rest.len() {
            match rest[i].as_str() {
                "--format" | "-f" => {
                    format = Some(ProjectFormat::from_str(
                        rest.get(i + 1)
                            .ok_or("--format needs a value (vivado|json)")?,
                    )?);
                    i += 2;
                }
                "--out" | "-o" => {
                    out = Some(PathBuf::from(rest.get(i + 1).ok_or("--out needs a value")?));
                    i += 2;
                }
                "--name" | "-n" => {
                    spec.name = rest.get(i + 1).ok_or("--name needs a value")?.clone();
                    i += 2;
                }
                "--top" => {
                    spec.top = Some(rest.get(i + 1).ok_or("--top needs a value")?.clone());
                    i += 2;
                }
                "--sim-top" => {
                    spec.sim_top = Some(rest.get(i + 1).ok_or("--sim-top needs a value")?.clone());
                    i += 2;
                }
                "--part" => {
                    spec.part = Some(rest.get(i + 1).ok_or("--part needs a value")?.clone());
                    i += 2;
                }
                other if other.starts_with("--") => {
                    return Err(format!("unknown flag '{}'", other));
                }
                f => {
                    spec.files.push(PathBuf::from(f));
                    i += 1;
                }
            }
        }
        let format = format.ok_or("missing --format (vivado|json)")?;
        if spec.files.is_empty() {
            return Err("no input files".to_string());
        }
        let mgr = build_manager(&opts)?;
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
