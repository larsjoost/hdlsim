//! Subcommand registry — the CLI extension seam.
//!
//! Each subcommand is a module under `commands/` implementing [`Command`] and
//! registered in [`COMMANDS`]. `dispatch` looks the first argument up in the
//! registry. Shared flags (`-L`, `--config`, `-I`) are parsed once in
//! [`extract_common`] and turned into a `LibraryManager` by [`build_manager`].

mod ast;
mod bundle;
mod check;
mod debug;
mod deps;
mod do_cmd;
mod fmt;
mod init;
mod lint;
pub mod lsp;
mod project;
mod regress;
mod run;

use hdl_lib::LibraryManager;
use std::path::{Path, PathBuf};

/// Extension seam: implement for a new subcommand, add one entry to [`COMMANDS`].
pub trait Command {
    fn name(&self) -> &'static str;
    /// Human-readable one-liner for `hdlsim --help`.
    fn about(&self) -> &'static str;
    /// Run with the arguments after the subcommand name. Returns the exit code.
    fn run(&self, args: &[String]) -> Result<i32, String>;
}

struct Help;

impl Command for Help {
    fn name(&self) -> &'static str {
        "--help"
    }
    fn about(&self) -> &'static str {
        "show this help"
    }
    fn run(&self, _args: &[String]) -> Result<i32, String> {
        println!("hdlsim 0.1.0 — self-contained VHDL + Verilog simulator");
        println!("usage: hdlsim <command> [args]");
        for c in COMMANDS {
            println!("  {:<12} {}", c.name(), c.about());
        }
        println!("common flags: -L name=path   map a logical library to a directory");
        println!("              --config FILE  library config (default: ./hdlsim.ini if present)");
        println!("              -I DIR         add an `include search directory");
        println!("              --bundle FILE  read the whole project from a reproducer .tar");
        Ok(0)
    }
}

/// Library/include options shared by `run`, `check`, and future commands.
#[derive(Default)]
pub struct CommonOpts {
    pub libs: Vec<(String, PathBuf)>,
    pub config: Option<PathBuf>,
    pub incdirs: Vec<PathBuf>,
    /// `--bundle repro.tar`: extract this reproducer bundle to a temp dir and
    /// resolve the whole project (config, libraries, include dirs) from there.
    pub bundle: Option<PathBuf>,
}

/// Split `args` into common library flags and the rest.
pub fn extract_common(args: &[String]) -> Result<(CommonOpts, Vec<String>), String> {
    let mut opts = CommonOpts::default();
    let mut rest = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-L" | "--lib" => {
                let v = args.get(i + 1).ok_or("-L needs a name=path value")?;
                let (n, p) = parse_lib_mapping(v)?;
                opts.libs.push((n, p));
                i += 2;
            }
            "--config" => {
                opts.config = Some(PathBuf::from(
                    args.get(i + 1).ok_or("--config needs a value")?,
                ));
                i += 2;
            }
            "-I" | "--incdir" => {
                opts.incdirs
                    .push(PathBuf::from(args.get(i + 1).ok_or("-I needs a value")?));
                i += 2;
            }
            "--bundle" => {
                opts.bundle = Some(PathBuf::from(
                    args.get(i + 1).ok_or("--bundle needs a value")?,
                ));
                i += 2;
            }
            other => {
                rest.push(other.to_string());
                i += 1;
            }
        }
    }
    Ok((opts, rest))
}

/// Parse a `name=path` library mapping (`-L work=./rtl`).
pub fn parse_lib_mapping(v: &str) -> Result<(String, PathBuf), String> {
    let (n, p) = v
        .split_once('=')
        .ok_or_else(|| format!("-L expects name=path, got '{}'", v))?;
    if n.is_empty() || p.is_empty() {
        return Err(format!("-L expects name=path, got '{}'", v));
    }
    Ok((n.to_string(), PathBuf::from(p)))
}

/// Build a `LibraryManager` from common options: config file (explicit
/// `--config`, else `./hdlsim.ini` if it exists), then `-L` mappings, then `-I`.
///
/// With `--bundle FILE` the bundle is extracted to a fresh temp dir first and
/// everything (config, libraries, include dirs) resolves from there; the
/// returned `Option<PathBuf>` is that extraction root (so callers can remap
/// file arguments into it), `None` otherwise.
pub fn build_manager(opts: &CommonOpts) -> Result<(LibraryManager, Option<PathBuf>), String> {
    let mut mgr = LibraryManager::new();
    mgr.use_env_cache(); // `.hdlsim-cache/` unless HDLSIM_NO_CACHE / HDLSIM_CACHE_DIR
    let mut root: Option<PathBuf> = None;
    if let Some(b) = &opts.bundle {
        let bytes =
            std::fs::read(b).map_err(|e| format!("cannot read bundle {}: {}", b.display(), e))?;
        let dir = std::env::temp_dir().join(format!("hdlsim_bundle_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        hdl_lib::bundle::extract_to(&bytes, &dir)
            .map_err(|e| format!("bad bundle {}: {}", b.display(), e))?;
        root = Some(dir.clone());
        let cfg = dir.join("hdlsim.ini");
        if cfg.is_file() {
            mgr.load_config(&cfg)?;
        }
        // Bundles record include dirs as inc/0, inc/1, ...
        let inc = dir.join("inc");
        if inc.is_dir() {
            let mut subs: Vec<PathBuf> = std::fs::read_dir(&inc)
                .map(|rd| rd.flatten().map(|e| e.path()).collect())
                .unwrap_or_default();
            subs.sort();
            for s in subs {
                if s.is_dir() {
                    mgr.add_incdir(s);
                }
            }
        }
    }
    let config = match &opts.config {
        Some(c) => Some(c.clone()),
        None => {
            let default = PathBuf::from("hdlsim.ini");
            default.is_file().then_some(default)
        }
    };
    if root.is_none() {
        if let Some(c) = &config {
            mgr.load_config(c)?;
        }
    }
    for (name, path) in &opts.libs {
        // With a bundle, relative -L paths resolve inside the extraction root.
        let p = match &root {
            Some(r) if !path.exists() => r.join(path),
            _ => path.clone(),
        };
        mgr.add_library_dir(name, &p);
    }
    for d in &opts.incdirs {
        let p = match &root {
            Some(r) if !d.exists() => r.join(d),
            _ => d.clone(),
        };
        mgr.add_incdir(p);
    }
    Ok((mgr, root))
}

/// Remap a CLI file argument into the bundle extraction root when it does not
/// exist on disk but does inside the bundle.
pub fn remap_into_bundle(f: &Path, root: &Option<PathBuf>) -> PathBuf {
    if let Some(r) = root {
        if !f.exists() && r.join(f).exists() {
            return r.join(f);
        }
    }
    f.to_path_buf()
}

/// Parse a time like `100ns`, `10us`, `1ms`, `500` (bare = ns) into femtoseconds.
pub fn parse_time(s: &str) -> Result<u64, String> {
    let (num, unit) = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .map(|i| (&s[..i], &s[i..]))
        .unwrap_or((s, ""));
    let v: f64 = num.parse().map_err(|_| format!("bad time value '{}'", s))?;
    let mult: f64 = match unit {
        "" | "ns" => 1e6,
        "fs" => 1.0,
        "ps" => 1e3,
        "us" | "µs" => 1e9,
        "ms" => 1e12,
        "s" => 1e15,
        _ => return Err(format!("unknown time unit '{}'", unit)),
    };
    Ok((v * mult) as u64)
}

/// Registry of all subcommands. Add one line per new command.
pub const COMMANDS: &[&dyn Command] = &[
    &Help,
    &run::Run,
    &check::Check,
    &ast::Ast,
    &init::Init,
    &project::Project,
    &debug::Debug,
    &bundle::Bundle,
    &regress::Regress,
    &lsp::Lsp,
    &fmt::Fmt,
    &lint::LintCmd,
    &do_cmd::DoCmd,
    &deps::Deps,
];

/// Dispatch `args` (without the binary name) to a registered command.
pub fn dispatch(args: &[String]) -> Result<i32, String> {
    match args.first() {
        None => COMMANDS[0].run(&[]),
        Some(name) => {
            let cmd = COMMANDS
                .iter()
                .find(|c| c.name() == name.as_str())
                .ok_or_else(|| format!("unknown command '{}'", name))?;
            cmd.run(&args[1..])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_lists_commands() {
        assert_eq!(dispatch(&["--help".to_string()]).unwrap(), 0);
    }

    #[test]
    fn unknown_command_is_an_error() {
        assert!(dispatch(&["frobnicate".to_string()]).is_err());
    }

    #[test]
    fn parses_times() {
        // Times are femtoseconds: 1 fs = 1, 1 ps = 1e3, 1 ns = 1e6, …
        assert_eq!(parse_time("100ns").unwrap(), 100_000_000);
        assert_eq!(parse_time("10us").unwrap(), 10_000_000_000);
        assert_eq!(parse_time("500").unwrap(), 500_000_000);
        assert_eq!(parse_time("2ms").unwrap(), 2_000_000_000_000);
        assert_eq!(parse_time("3ps").unwrap(), 3_000);
        assert!(parse_time("10xs").is_err());
    }

    #[test]
    fn extracts_common_flags() {
        let args = vec![
            "-L".to_string(),
            "work=./rtl".to_string(),
            "--lib".to_string(),
            "ieee=./ieee".to_string(),
            "-I".to_string(),
            "inc".to_string(),
            "tb.v".to_string(),
        ];
        let (opts, rest) = extract_common(&args).unwrap();
        assert_eq!(opts.libs.len(), 2);
        assert_eq!(opts.libs[0].0, "work");
        assert_eq!(opts.incdirs, vec![PathBuf::from("inc")]);
        assert_eq!(rest, vec!["tb.v".to_string()]);
        assert!(parse_lib_mapping("noequals").is_err());
    }
}
