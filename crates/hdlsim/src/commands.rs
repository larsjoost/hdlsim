//! Subcommand registry — the CLI extension seam.
//!
//! Each subcommand is a module under `commands/` implementing [`Command`] and
//! registered in [`COMMANDS`]. `dispatch` looks the first argument up in the
//! registry. A command parses its args with a `clap::Parser` derive via
//! [`parse_with`], flattening in [`CommonArgs`] for the shared `-L`/`--config`/
//! `-I`/`--bundle` flags (see `commands/deps.rs` for the pattern); the parsed
//! [`CommonOpts`] are turned into a `LibraryManager` by [`build_manager`].
//! `--color`/`--no-color` are handled globally in `main.rs` because they must
//! work before the subcommand name.

mod analyze;
mod assist;
mod ast;
mod bundle;
mod check;
mod completions;
mod debug;
mod deps;
mod do_cmd;
mod doc;
mod doctor;
mod fmt;
mod gen;
mod init;
mod lint;
pub mod lsp;
mod project;
mod prove;
mod regress;
mod run;
mod wave;

use anyhow::Context as _;
use hdl_lib::LibraryManager;
use std::path::{Path, PathBuf};

/// Extension seam: implement for a new subcommand, add one entry to [`COMMANDS`].
pub trait Command {
    fn name(&self) -> &'static str;
    /// Human-readable one-liner for `hdlsim --help`.
    fn about(&self) -> &'static str;
    /// Run with the arguments after the subcommand name. Returns the exit code.
    fn run(&self, args: &[String]) -> anyhow::Result<i32>;
}

struct Help;

impl Command for Help {
    fn name(&self) -> &'static str {
        "--help"
    }
    fn about(&self) -> &'static str {
        "show this help"
    }
    fn run(&self, _args: &[String]) -> anyhow::Result<i32> {
        println!(
            "hdlsim {} — self-contained VHDL + Verilog simulator",
            env!("CARGO_PKG_VERSION")
        );
        println!("usage: hdlsim <command> [args]");
        for c in COMMANDS {
            println!("  {:<12} {}", c.name(), c.about());
        }
        println!("common flags: -L name=path   map a logical library to a directory");
        println!("              --config FILE  library config (default: ./hdlsim.ini if present)");
        println!("              -I DIR         add an `include search directory");
        println!("              --bundle FILE  read the whole project from a reproducer .tar");
        println!();
        println!("  Global flags (before or after the command):");
        println!("    --color auto|always|never   colorize errors/warnings (default: auto =");
        println!(
            "                                 color when stderr is a terminal; env HDLSIM_COLOR,"
        );
        println!("                                 NO_COLOR also apply)");
        println!("    --no-color                   disable color output");
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

/// Shared library/include/bundle flags. Commands that compile sources flatten
/// this into their args struct: `#[command(flatten)] common: CommonArgs`.
#[derive(clap::Args, Clone, Default)]
pub struct CommonArgs {
    /// Map a logical library to a directory: -L name=path.
    #[arg(long, short = 'L', value_name = "name=path")]
    pub lib: Vec<String>,
    /// Library config file (default: ./hdlsim.ini if present).
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// Add an `include search directory.
    #[arg(long, short = 'I')]
    pub incdir: Vec<PathBuf>,
    /// Read the whole project from a reproducer .tar.
    #[arg(long)]
    pub bundle: Option<PathBuf>,
}

impl CommonArgs {
    /// Validate (`name=path`) and convert into [`CommonOpts`].
    pub fn into_opts(self) -> anyhow::Result<CommonOpts> {
        let mut opts = CommonOpts::default();
        for v in &self.lib {
            opts.libs.push(parse_lib_mapping(v)?);
        }
        opts.config = self.config;
        opts.incdirs = self.incdir;
        opts.bundle = self.bundle;
        Ok(opts)
    }
}

/// Parse a subcommand's args with its `clap::Parser` derive. `cmd` is the
/// subcommand name, shown in usage/help as `hdlsim <cmd>`. On `--help` or a
/// parse error the message is printed (stdout for help, stderr for errors) and
/// the exit code is returned as `Err` (0 for help, 2 for errors).
pub fn parse_with<T: clap::Parser>(cmd: &str, args: &[String]) -> Result<T, i32> {
    let argv = std::iter::once(format!("hdlsim {}", cmd)).chain(args.iter().cloned());
    match T::try_parse_from(argv) {
        Ok(v) => Ok(v),
        Err(e) => {
            let _ = e.print();
            Err(if e.use_stderr() { 2 } else { 0 })
        }
    }
}

/// Parse a `name=path` library mapping (`-L work=./rtl`).
pub fn parse_lib_mapping(v: &str) -> anyhow::Result<(String, PathBuf)> {
    let (n, p) = v
        .split_once('=')
        .ok_or_else(|| anyhow::anyhow!("-L expects name=path, got '{}'", v))?;
    if n.is_empty() || p.is_empty() {
        return Err(anyhow::anyhow!("-L expects name=path, got '{}'", v));
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
pub fn build_manager(opts: &CommonOpts) -> anyhow::Result<(LibraryManager, Option<PathBuf>)> {
    let mut mgr = LibraryManager::new();
    mgr.use_env_cache(); // `.hdlsim-cache/` unless HDLSIM_NO_CACHE / HDLSIM_CACHE_DIR
    let mut root: Option<PathBuf> = None;
    if let Some(b) = &opts.bundle {
        let bytes =
            std::fs::read(b).with_context(|| format!("cannot read bundle {}", b.display()))?;
        let dir = std::env::temp_dir().join(format!("hdlsim_bundle_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        hdl_lib::bundle::extract_to(&bytes, &dir)
            .map_err(|e| anyhow::anyhow!("bad bundle {}: {}", b.display(), e))?;
        root = Some(dir.clone());
        let cfg = dir.join("hdlsim.ini");
        if cfg.is_file() {
            mgr.load_config(&cfg).map_err(anyhow::Error::msg)?;
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
    let mut config_loaded = false;
    if root.is_none() {
        if let Some(c) = &config {
            mgr.load_config(c).map_err(anyhow::Error::msg)?;
            config_loaded = true;
        }
    }
    // Out of the box: with no config file and no -L mappings, index the
    // current directory as library `work` (recursively), so `use work.foo.all;`
    // and `entity work.foo` resolve in any project without setup.
    if root.is_none() && !config_loaded && opts.libs.is_empty() {
        mgr.add_library_dir("work", Path::new("."));
    }
    // The project root (cwd, or the bundle extraction root) is always a
    // fallback search root for units no library maps.
    mgr.add_search_root(root.clone().unwrap_or_else(|| PathBuf::from(".")));
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
pub fn parse_time(s: &str) -> anyhow::Result<u64> {
    let (num, unit) = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .map(|i| (&s[..i], &s[i..]))
        .unwrap_or((s, ""));
    let v: f64 = num
        .parse()
        .map_err(|_| anyhow::anyhow!("bad time value '{}'", s))?;
    let mult: f64 = match unit {
        "" | "ns" => 1e6,
        "fs" => 1.0,
        "ps" => 1e3,
        "us" | "µs" => 1e9,
        "ms" => 1e12,
        "s" => 1e15,
        _ => return Err(anyhow::anyhow!("unknown time unit '{}'", unit)),
    };
    Ok((v * mult) as u64)
}

/// Registry of all subcommands. Add one line per new command.
pub const COMMANDS: &[&dyn Command] = &[
    &Help,
    &run::Run,
    &wave::Wave,
    &check::Check,
    &ast::Ast,
    &init::Init,
    &project::Project,
    &prove::Prove,
    &debug::Debug,
    &bundle::Bundle,
    &regress::Regress,
    &lsp::Lsp,
    &fmt::Fmt,
    &lint::LintCmd,
    &do_cmd::DoCmd,
    &deps::Deps,
    &assist::Assist,
    &assist::Explain,
    &gen::Gen,
    &analyze::Analyze,
    &doc::Doc,
    &doctor::Doctor,
    &completions::Completions,
];

/// Dispatch `args` (without the binary name) to a registered command.
/// Pull global color flags (`--color <auto|always|never>`, `--color=<...>`,
/// `--no-color`) out of `args` and apply them to the diagnostic renderer.
/// Returns the remaining args. Env fallback: `HDLSIM_COLOR` overrides the
/// tty auto-detection, `NO_COLOR` disables color entirely.
pub fn apply_color_args(
    args: &[String],
) -> (Vec<String>, Option<hdl_syntax::diagnostic::ColorMode>) {
    let mut rest = Vec::new();
    let mut mode = None;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--no-color" {
            mode = Some(hdl_syntax::diagnostic::ColorMode::Never);
            i += 1;
        } else if let Some(v) = a.strip_prefix("--color=") {
            mode = Some(parse_color_mode(v));
            i += 1;
        } else if a == "--color" {
            let v = args.get(i + 1).map(|s| s.as_str()).unwrap_or("auto");
            mode = Some(parse_color_mode(v));
            i += if args.get(i + 1).is_some() { 2 } else { 1 };
        } else {
            rest.push(a.clone());
            i += 1;
        }
    }
    (rest, mode)
}

fn parse_color_mode(v: &str) -> hdl_syntax::diagnostic::ColorMode {
    match v {
        "always" | "yes" | "1" => hdl_syntax::diagnostic::ColorMode::Always,
        "never" | "no" | "0" => hdl_syntax::diagnostic::ColorMode::Never,
        _ => hdl_syntax::diagnostic::ColorMode::Auto,
    }
}

pub fn dispatch(args: &[String]) -> anyhow::Result<i32> {
    match args.first() {
        None => COMMANDS[0].run(&[]),
        Some(name) => {
            let cmd = COMMANDS
                .iter()
                .find(|c| c.name() == name.as_str())
                .ok_or_else(|| anyhow::anyhow!("unknown command '{}'", name))?;
            cmd.run(&args[1..])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn help_lists_commands() {
        assert_eq!(dispatch(&["--help".to_string()]).unwrap(), 0);
    }

    #[test]
    fn unknown_command_is_an_error() {
        assert!(dispatch(&["frobnicate".to_string()]).is_err());
    }

    #[test]
    fn clap_help_and_parse_error_exit_codes() {
        // --help prints usage and exits 0; parse errors print to stderr, exit 2.
        assert_eq!(
            dispatch(&["deps".to_string(), "--help".to_string()]).unwrap(),
            0
        );
        assert_eq!(dispatch(&["deps".to_string()]).unwrap(), 2);
        assert_eq!(
            dispatch(&["doctor".to_string(), "extra".to_string()]).unwrap(),
            2
        );
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
    fn common_args_parse_and_validate() {
        #[derive(Parser)]
        struct T {
            #[command(flatten)]
            common: CommonArgs,
            file: String,
        }
        let t = T::try_parse_from([
            "x",
            "-L",
            "work=./rtl",
            "--lib",
            "ieee=./ieee",
            "-I",
            "inc",
            "--bundle",
            "b.tar",
            "tb.v",
        ])
        .unwrap();
        assert_eq!(t.file, "tb.v");
        let opts = t.common.into_opts().unwrap();
        assert_eq!(opts.libs.len(), 2);
        assert_eq!(opts.libs[0].0, "work");
        assert_eq!(opts.incdirs, vec![PathBuf::from("inc")]);
        assert_eq!(opts.bundle, Some(PathBuf::from("b.tar")));
        // Bad name=path is a validation error, not a parse error.
        let t = T::try_parse_from(["x", "-L", "noequals", "tb.v"]).unwrap();
        assert!(t.common.into_opts().is_err());
        assert!(parse_lib_mapping("noequals").is_err());
    }
}
