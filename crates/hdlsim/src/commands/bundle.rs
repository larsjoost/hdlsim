//! `hdlsim bundle` — pack a project into a single `.tar` reproducer that can be
//! attached to a GitHub issue, so a failure can be reproduced with one file.
//!
//! The bundle contains the command-line source files (`src/`), every mapped
//! library directory (`libs/<name>/`), include dirs (`inc/<n>/`), the original
//! config (`hdlsim.ini.orig`), a generated `hdlsim.ini` pointing into the
//! bundle, and a `REPRO.md` with the exact reproduce commands.
//!
//! Consume it with `hdlsim run --bundle repro.tar --top tb` (also `check`,
//! `debug`, `project`) or plain `tar -xf`.

use super::{parse_with, Command, CommonArgs};
use clap::Parser;
use hdl_lib::bundle::{write_tar, BundleEntry};
use hdl_lib::parse_library_config;
use std::path::{Component, Path, PathBuf};

/// Total bundle size cap (sources are small; this guards against packing
/// e.g. a `target/` dir by accident).
const SIZE_CAP: u64 = 256 * 1024 * 1024;

#[derive(Parser)]
#[command(
    about = "pack a project into a single .tar reproducer: hdlsim bundle [--out repro.tar] [--top tb] [--label text] [files...]"
)]
struct BundleArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// Output tar file (default: hdlsim-repro.tar).
    #[arg(long, short = 'o')]
    out: Option<PathBuf>,
    #[arg(long)]
    top: Option<String>,
    /// Free-text label recorded in REPRO.md.
    #[arg(long, default_value = "")]
    label: String,
    /// Source files.
    files: Vec<PathBuf>,
}

pub struct Bundle;

impl Command for Bundle {
    fn name(&self) -> &'static str {
        "bundle"
    }
    fn about(&self) -> &'static str {
        "pack the project into one .tar reproducer for a bug report: hdlsim bundle --out repro.tar [--top tb] [files]"
    }
    fn run(&self, args: &[String]) -> anyhow::Result<i32> {
        let a = match parse_with::<BundleArgs>("bundle", args) {
            Ok(a) => a,
            Err(code) => return Ok(code),
        };
        let opts = a.common.into_opts()?;
        let out = a.out.unwrap_or_else(|| PathBuf::from("hdlsim-repro.tar"));
        let top = a.top;
        let label = a.label;
        let files: Vec<PathBuf> = a.files;

        // Library mappings: from the config file (explicit --config or
        // ./hdlsim.ini), then -L flags; first mapping wins per name.
        let cfg = match &opts.config {
            Some(c) => Some(c.clone()),
            None => {
                let d = PathBuf::from("hdlsim.ini");
                d.is_file().then_some(d)
            }
        };
        let mut libs: Vec<(String, PathBuf)> = Vec::new();
        if let Some(c) = &cfg {
            let text = std::fs::read_to_string(c)
                .map_err(|e| anyhow::anyhow!("cannot read {}: {}", c.display(), e))?;
            let base = c.parent().unwrap_or_else(|| Path::new("."));
            for (n, p) in parse_library_config(&text, base) {
                if !libs.iter().any(|(e, _)| e == &n) {
                    libs.push((n, p));
                }
            }
        }
        for (n, p) in &opts.libs {
            if !libs.iter().any(|(e, _)| e == n) {
                libs.push((n.clone(), p.clone()));
            }
        }

        let mut entries: Vec<BundleEntry> = Vec::new();
        let mut size: u64 = 0;
        fn push(
            entries: &mut Vec<BundleEntry>,
            size: &mut u64,
            path: String,
            data: Vec<u8>,
        ) -> anyhow::Result<()> {
            *size += data.len() as u64;
            if *size > SIZE_CAP {
                return Err(anyhow::anyhow!(
                    "bundle exceeds 256 MB — refusing to pack (is a build dir mapped?)"
                ));
            }
            entries.push(BundleEntry { path, data });
            Ok(())
        }

        // src/: explicit files that are not inside any mapped library dir.
        let lib_dirs: Vec<PathBuf> = libs
            .iter()
            .map(|(_, p)| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()))
            .collect();
        let mut src_files: Vec<String> = Vec::new();
        for f in &files {
            let canon = std::fs::canonicalize(f).unwrap_or_else(|_| f.clone());
            if lib_dirs.iter().any(|d| canon.starts_with(d)) {
                continue; // already packed via libs/<name>/
            }
            let data = std::fs::read(f)
                .map_err(|e| anyhow::anyhow!("cannot read {}: {}", f.display(), e))?;
            let rel = rel_of(
                f,
                &std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            );
            src_files.push(rel.clone());
            push(&mut entries, &mut size, format!("src/{}", rel), data)
                .map_err(anyhow::Error::msg)?;
        }

        // libs/<name>/: full recursive copy of each library directory.
        for (n, p) in &libs {
            if !p.is_dir() {
                continue;
            }
            let safe = sanitize(n);
            walk_files(p, p, &format!("libs/{}", safe), &mut entries, &mut size)
                .map_err(anyhow::Error::msg)?;
        }

        // inc/<i>/: include search dirs.
        for (idx, d) in opts.incdirs.iter().enumerate() {
            if d.is_dir() {
                walk_files(d, d, &format!("inc/{}", idx), &mut entries, &mut size)
                    .map_err(anyhow::Error::msg)?;
            }
        }

        // Generated hdlsim.ini pointing into the bundle.
        let mut ini = String::from(
            "# Generated by `hdlsim bundle` — library paths are inside this archive.\n[Library]\n",
        );
        if !src_files.is_empty() {
            ini.push_str("bundle_src = src\n");
        }
        for (n, _) in &libs {
            ini.push_str(&format!("{} = libs/{}\n", n, sanitize(n)));
        }
        push(
            &mut entries,
            &mut size,
            "hdlsim.ini".to_string(),
            ini.into_bytes(),
        )
        .map_err(anyhow::Error::msg)?;

        // Original config for reference.
        if let Some(c) = &cfg {
            if let Ok(text) = std::fs::read(c) {
                push(&mut entries, &mut size, "hdlsim.ini.orig".to_string(), text)
                    .map_err(anyhow::Error::msg)?;
            }
        }

        // REPRO.md with everything needed to reproduce and file an issue.
        let argv: Vec<String> = std::env::args().collect();
        let repro_files = if files.is_empty() {
            "(none — everything resolves from the bundled libraries)".to_string()
        } else {
            files
                .iter()
                .map(|f| f.display().to_string())
                .collect::<Vec<_>>()
                .join(" ")
        };
        let run_cmd = match &top {
            Some(t) => format!("hdlsim run --top {}", t),
            None => format!("hdlsim run {}", repro_files),
        };
        let repro = format!(
            "# hdlsim reproducer bundle\n\n\
             - created: {} UTC\n\
             - hdlsim version: {}\n\
             - original command: {}\n\
             - top: {}\n\
             - problem: {}\n\n\
             ## Contents\n\n\
             - `src/` — source files passed on the command line\n\
             - `libs/<name>/` — each library directory (hdlsim.ini / -L mappings)\n\
             - `inc/` — `include search directories\n\
             - `hdlsim.ini` — generated library map for this bundle\n\
             - `hdlsim.ini.orig` — the original library config, if any\n\n\
             ## Reproduce\n\n\
             ```\n\
             tar -xf {out} -C repro && cd repro\n\
             {run}\n\
             ```\n\n\
             or directly from the bundle, without extracting:\n\n\
             ```\n\
             {run} --bundle {out}\n\
             ```\n\n\
             Please attach this .tar to a GitHub issue:\n\
             https://github.com/larsjoost/hdlsim/issues/new\n",
            utc_now(),
            env!("CARGO_PKG_VERSION"),
            argv.join(" "),
            top.clone().unwrap_or_else(|| "<none>".to_string()),
            if label.is_empty() {
                "hdlsim failed or produced wrong results on this project".to_string()
            } else {
                label.clone()
            },
            out = out
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "repro.tar".to_string()),
            run = run_cmd,
        );
        push(
            &mut entries,
            &mut size,
            "REPRO.md".to_string(),
            repro.into_bytes(),
        )
        .map_err(anyhow::Error::msg)?;

        let tar = write_tar(&entries).map_err(anyhow::Error::msg)?;
        std::fs::write(&out, &tar)
            .map_err(|e| anyhow::anyhow!("cannot write {}: {}", out.display(), e))?;
        println!(
            "hdlsim: wrote {} ({} files, {} bytes) — attach it to a GitHub issue",
            out.display(),
            entries.len(),
            tar.len()
        );
        println!(
            "hdlsim: reproduce with: hdlsim run --bundle {} {}",
            out.display(),
            top.map(|t| format!("--top {}", t)).unwrap_or_default()
        );
        Ok(0)
    }
}

/// Path of `path` relative to `base`, `/`-separated; falls back to the file name.
fn rel_of(path: &Path, base: &Path) -> String {
    let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let cbase = std::fs::canonicalize(base).unwrap_or_else(|_| base.to_path_buf());
    let rel = canon.strip_prefix(&cbase).unwrap_or(path);
    let parts: Vec<String> = rel
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => s.to_str().map(String::from),
            _ => None,
        })
        .collect();
    if parts.is_empty() {
        path.file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".to_string())
    } else {
        parts.join("/")
    }
}

/// Make a library name safe for use as a bundle path component.
fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Recursively add every regular file under `dir` as `prefix/<rel>` entries.
fn walk_files(
    dir: &Path,
    base: &Path,
    prefix: &str,
    entries: &mut Vec<BundleEntry>,
    size: &mut u64,
) -> anyhow::Result<()> {
    let mut children: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(rd) => rd.flatten().map(|e| e.path()).collect(),
        Err(_) => return Ok(()), // unreadable dir: skip
    };
    children.sort();
    for p in children {
        if p.is_dir() {
            walk_files(&p, base, prefix, entries, size)?;
        } else if p.is_file() {
            let data = std::fs::read(&p)
                .map_err(|e| anyhow::anyhow!("cannot read {}: {}", p.display(), e))
                .map_err(anyhow::Error::msg)?;
            *size += data.len() as u64;
            if *size > SIZE_CAP {
                return Err(anyhow::anyhow!("bundle exceeds 256 MB — refusing to pack"));
            }
            entries.push(BundleEntry {
                path: format!("{}/{}", prefix, rel_of(&p, base)),
                data,
            });
        }
    }
    Ok(())
}

/// Current UTC time as `YYYY-MM-DD HH:MM:SS` (std-only civil-from-days).
fn utc_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", y, mo, d, h, m, s)
}
