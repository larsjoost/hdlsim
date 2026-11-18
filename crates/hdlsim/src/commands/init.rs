//! `hdlsim init` — scan the current project and write an `hdlsim.ini` library
//! config, so design units resolve instantly (indexed) instead of via the
//! on-demand project search.

use super::{parse_with, Command};
use clap::Parser;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One library name maps to exactly one directory: when several dirs share a
/// name (e.g. the source dir and a deep build-tree copy under `vivado/...`),
/// keep the shallowest path; ties keep the first in sorted walk order.
fn prefer_dir(current: &Path, candidate: &Path) -> bool {
    candidate.components().count() < current.components().count()
}

/// Guess a logical library name from a directory name: lowercase, invalid
/// identifier chars become `_`, and a non-identifier start gets a `lib_`
/// prefix (e.g. `scint_scan_integrator` stays as-is, `2nd` -> `lib_2nd`).
fn library_name_from_dir(dir: &Path) -> Option<String> {
    let raw = dir.file_name()?.to_string_lossy().to_ascii_lowercase();
    let mut name: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if name.is_empty() {
        return None;
    }
    if !name
        .chars()
        .next()
        .map_or(false, |c| c.is_ascii_alphabetic() || c == '_')
    {
        name = format!("lib_{name}");
    }
    // Reserved / built-in names are not user libraries.
    if matches!(name.as_str(), "work" | "std" | "ieee" | "ieee_proposed") {
        return None;
    }
    Some(name)
}

#[derive(Parser)]
#[command(
    about = "scan the project and write an hdlsim.ini library config in the current directory"
)]
struct InitArgs {
    /// Overwrite an existing hdlsim.ini.
    #[arg(long)]
    force: bool,
    /// Project root to scan (default: current directory).
    #[arg(long, default_value = ".")]
    root: PathBuf,
    /// Print scan progress to stderr.
    #[arg(long)]
    progress: bool,
}

pub struct Init;

impl Command for Init {
    fn name(&self) -> &'static str {
        "init"
    }
    fn about(&self) -> &'static str {
        "scan the project and write an hdlsim.ini library config in the current directory"
    }
    fn run(&self, args: &[String]) -> anyhow::Result<i32> {
        let a = match parse_with::<InitArgs>("init", args) {
            Ok(a) => a,
            Err(code) => return Ok(code),
        };
        let root = a.root.canonicalize().unwrap_or_else(|_| a.root.clone());
        let path = Path::new("hdlsim.ini");
        if path.exists() && !a.force {
            return Err(anyhow::anyhow!(
                "hdlsim.ini already exists (use --force to overwrite)"
            ));
        }
        // Scan the project for source files (paths only — no parsing and no
        // file reads, so the scan itself is fast). Each directory holding
        // VHDL sources becomes a logical library named after the directory's
        // last component (`library X;` clauses in the files are often stale
        // copy-paste and not reliable); the root is covered by `work = .`.
        let progress = a.progress;
        let files = if progress {
            hdl_lib::providers::walk_source_files_with(&root, |dir, n| {
                eprint!("\r  scanning {} ({} files)   ", dir.display(), n);
            })
        } else {
            hdl_lib::providers::walk_source_files(&root)
        };
        if progress {
            eprintln!();
        }
        let mut lib_dirs: BTreeMap<String, PathBuf> = BTreeMap::new();
        let mut n_vhdl = 0usize;
        let mut n_verilog = 0usize;
        let n_files = files.len();
        for file in &files {
            let ext = file
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase());
            match ext.as_deref() {
                Some("vhd") | Some("vhdl") => n_vhdl += 1,
                Some(_) => n_verilog += 1,
                None => {}
            }
            if !matches!(ext.as_deref(), Some("vhd") | Some("vhdl")) {
                continue;
            }
            let dir = file
                .parent()
                .and_then(|p| p.strip_prefix(&root).ok())
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| PathBuf::from("."));
            if dir.as_os_str().is_empty() || dir == Path::new(".") {
                continue; // project root is covered by `work = .`
            }
            if let Some(name) = library_name_from_dir(&dir) {
                match lib_dirs.get(&name) {
                    Some(cur) if !prefer_dir(cur, &dir) => {}
                    _ => {
                        lib_dirs.insert(name, dir);
                    }
                }
            }
        }
        // `work = .` indexes the whole tree recursively; the per-library lines
        // additionally map each logical library name to one source directory
        // (named after the directory).
        let mut ini = String::from(
            "# hdlsim library configuration (generated by `hdlsim init`)\n[Library]\nwork = .\n",
        );
        for (lib, d) in &lib_dirs {
            let rel = if d.as_os_str().is_empty() || d == Path::new(".") {
                ".".to_string()
            } else {
                d.display().to_string()
            };
            ini.push_str(&format!("{lib} = {rel}\n"));
        }
        std::fs::write(path, &ini)
            .map_err(|e| anyhow::anyhow!("cannot write hdlsim.ini: {}", e))?;
        println!(
            "created hdlsim.ini — {} source files ({} VHDL, {} Verilog) under {}, {} logical librar{}",
            n_files,
            n_vhdl,
            n_verilog,
            root.display(),
            lib_dirs.len(),
            if lib_dirs.len() == 1 { "y" } else { "ies" }
        );
        if lib_dirs.is_empty() {
            println!("no VHDL sub-directories found; `work = .` covers the whole tree");
        } else {
            println!("libraries: {}", {
                let mut v: Vec<&String> = lib_dirs.keys().collect();
                v.sort();
                v.into_iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            });
        }
        println!("resolution is now indexed — no on-demand search needed");
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_name_from_dir_uses_last_component() {
        assert_eq!(
            library_name_from_dir(Path::new("hw/hdl/sccore/processor")),
            Some("processor".to_string())
        );
        assert_eq!(
            library_name_from_dir(Path::new("hw/Hdl/SCInt")),
            Some("scint".to_string())
        );
        assert_eq!(
            library_name_from_dir(Path::new("hw/2nd-gen")),
            Some("lib_2nd_gen".to_string())
        );
        assert_eq!(library_name_from_dir(Path::new(".")), None);
        assert_eq!(library_name_from_dir(Path::new("ieee")), None);
        assert_eq!(library_name_from_dir(Path::new("work")), None);
    }
}

#[cfg(test)]
mod dedupe_tests {
    use super::*;

    #[test]
    fn prefer_dir_keeps_shallowest() {
        let src = Path::new("hw/hdl/sccore/acmb_azimuth_combiner");
        let copy = Path::new(
            "vivado/pcm/rtl/project.srcs/sources_1/imports/hw/hdl/sccore/acmb_azimuth_combiner",
        );
        assert!(prefer_dir(src, copy) == false);
        assert!(prefer_dir(copy, src));
        // Equal depth: keep the current one (first in sorted walk order).
        assert!(!prefer_dir(Path::new("a/x"), Path::new("b/x")));
    }
}
