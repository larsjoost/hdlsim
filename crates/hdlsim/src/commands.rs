//! Subcommand registry — the CLI extension seam.
//!
//! Each subcommand is a module under `commands/` implementing [`Command`] and
//! registered in [`COMMANDS`]. `dispatch` looks the first argument up in the
//! registry. Shared flags (`-L`, `--config`, `-I`) are parsed once in
//! [`extract_common`] and turned into a `LibraryManager` by [`build_manager`].

mod ast;
mod check;
mod debug;
mod init;
mod project;
mod run;

use hdl_lib::LibraryManager;
use std::path::PathBuf;

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
        Ok(0)
    }
}

/// Library/include options shared by `run`, `check`, and future commands.
#[derive(Default)]
pub struct CommonOpts {
    pub libs: Vec<(String, PathBuf)>,
    pub config: Option<PathBuf>,
    pub incdirs: Vec<PathBuf>,
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
pub fn build_manager(opts: &CommonOpts) -> Result<LibraryManager, String> {
    let mut mgr = LibraryManager::new();
    let config = match &opts.config {
        Some(c) => Some(c.clone()),
        None => {
            let default = PathBuf::from("hdlsim.ini");
            default.is_file().then_some(default)
        }
    };
    if let Some(c) = &config {
        mgr.load_config(c)?;
    }
    for (name, path) in &opts.libs {
        mgr.add_library_dir(name, path);
    }
    for d in &opts.incdirs {
        mgr.add_incdir(d.clone());
    }
    Ok(mgr)
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
