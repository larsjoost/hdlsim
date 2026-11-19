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
pub mod report;
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
        println!("       hdlsim help <command>   detailed help for one command");
        for c in COMMANDS {
            println!("  {:<12} {}", c.name(), c.about());
        }
        println!("common flags: -L name=path   map a logical library to a directory");
        println!("              --config FILE  library config (default: ./hdlsim.ini if present)");
        println!("              -I DIR         add an `include search directory");
        println!("              -S DIR         add DIR to the on-demand project search roots");
        println!(
            "              --progress     show library indexing / resolution progress on stderr"
        );
        println!(
            "              --lib-root DIR treat each subdir of DIR as a library named after it"
        );
        println!(
            "                             (Xilinx auto-detect: HDLSIM_XILINX_LIBS, XILINX_VIVADO,"
        );
        println!("                              XILINX_HLS -> data/vhdl/src)");
        println!("              --bundle FILE  read the whole project from a reproducer .tar");
        println!();
        println!("  Global flags (before or after the command):");
        println!("    --color auto|always|never   colorize errors/warnings (default: auto =");
        println!(
            "                                 color when stderr is a terminal; env HDLSIM_COLOR,"
        );
        println!("                                 NO_COLOR also apply)");
        println!("    --no-color                   disable color output");
        println!(
            "    --report \"msg\"             on failure, file a GitHub issue with a reproducer"
        );
        println!(
            "                                 bundle with the captured command output (uses the gh"
        );
        println!(
            "                                 CLI; large bundles are uploaded as a secret gist and"
        );
        println!(
            "                                 linked; --report-dry-run to preview, also works without"
        );
        println!("                                 a message)");
        Ok(0)
    }
}

/// Library/include options shared by `run`, `check`, and future commands.
#[derive(Default)]
pub struct CommonOpts {
    pub libs: Vec<(String, PathBuf)>,
    pub config: Option<PathBuf>,
    pub incdirs: Vec<PathBuf>,
    /// `--lib-root DIR`: each subdirectory of DIR becomes a library named
    /// after the subdir.
    pub lib_roots: Vec<PathBuf>,
    /// `-S DIR`: extra on-demand project search roots (no library name).
    pub search_dirs: Vec<PathBuf>,
    /// Print indexing/resolution progress to stderr.
    pub progress: bool,
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
    /// Treat each subdirectory of DIR as a logical library named after the
    /// subdir (e.g. --lib-root $XILINX_VIVADO/data/vhdl/src maps unisim,
    /// unimacro, secureip, ...). Repeatable.
    #[arg(long, value_name = "DIR")]
    pub lib_root: Vec<PathBuf>,
    /// Add DIR to the on-demand project search roots (no library name).
    /// Repeatable.
    #[arg(long, short = 'S', value_name = "DIR")]
    pub search_dir: Vec<PathBuf>,
    /// Print library indexing / unit-resolution progress to stderr.
    #[arg(long)]
    pub progress: bool,
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
        opts.lib_roots = self.lib_root;
        opts.search_dirs = self.search_dir;
        opts.progress = self.progress;
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
    mgr.set_progress(opts.progress);
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
        None => find_config_upwards(Path::new(".")),
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
    // Library roots (--lib-root and Xilinx env auto-detection): each subdir
    // with sources becomes a library named after the subdir. Explicit ini/-L
    // mappings come first, so they win on unit-name collisions.
    for lr in &opts.lib_roots {
        mgr.add_lib_root(lr);
    }
    for xr in hdl_lib::providers::xilinx_lib_roots_from_env() {
        mgr.add_lib_root(&xr);
    }
    for d in &opts.search_dirs {
        mgr.add_search_root(d.clone());
    }
    Ok((mgr, root))
}

/// Default config lookup: `./hdlsim.ini`, then each parent directory (like git
/// searching upward for `.git`), so running hdlsim from a project
/// subdirectory still picks up the project root's `hdlsim.ini`. Relative
/// paths inside the ini resolve against the ini's own directory.
fn find_config_upwards(start: &Path) -> Option<PathBuf> {
    let mut dir = start.canonicalize().ok()?;
    loop {
        let cand = dir.join("hdlsim.ini");
        if cand.is_file() {
            return Some(cand);
        }
        if !dir.pop() {
            return None;
        }
    }
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

/// Pick the top module when `--top` is omitted. VHDL architectures lower to
/// `entity/arch` variant modules while the plain entity module carries the
/// default architecture's body, so only names without `/` are candidates.
/// Exactly one candidate (or one module overall) wins; otherwise the caller
/// must pass `--top`.
pub fn pick_default_top(design: &hdl_ir::Design) -> anyhow::Result<String> {
    let mut tops: Vec<&str> = design
        .modules
        .iter()
        .map(|m| m.name.as_str())
        .filter(|n| !n.contains('/'))
        .collect();
    tops.sort_unstable();
    tops.dedup();
    let names: Vec<&str> = if tops.is_empty() {
        design.modules.iter().map(|m| m.name.as_str()).collect()
    } else {
        tops
    };
    match names.len() {
        1 => Ok(names[0].to_string()),
        _ => Err(anyhow::anyhow!(
            "multiple modules ({}); pick one with --top",
            names.join(", ")
        )),
    }
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

/// `hdlsim help <command>` — detailed per-command documentation.
struct HelpCmd;

impl Command for HelpCmd {
    fn name(&self) -> &'static str {
        "help"
    }
    fn about(&self) -> &'static str {
        "show detailed help for a command: hdlsim help <command>"
    }
    fn run(&self, args: &[String]) -> anyhow::Result<i32> {
        let Some(name) = args.first() else {
            println!("usage: hdlsim help <command>\n");
            return Help.run(&[]);
        };
        let cmd = COMMANDS
            .iter()
            .find(|c| c.name() == name.as_str())
            .ok_or_else(|| anyhow::anyhow!("unknown command '{name}' (try 'hdlsim --help')"))?;
        println!("hdlsim {} — {}", cmd.name(), cmd.about());
        if let Some(long) = command_long_help(cmd.name()) {
            println!("\n{long}");
        }
        println!(
            "\nflags (`hdlsim {} --help` shows the same list):",
            cmd.name()
        );
        cmd.run(&["--help".to_string()])
    }
}

/// Detailed long-form documentation per command, shown by `hdlsim help <cmd>`.
fn command_long_help(name: &str) -> Option<&'static str> {
    Some(match name {
        "run" => "Compile the given sources and simulate the testbench --top until it\nstops (or --max-time). Waves: --vcd/--hwf/--fst files, --wave/--live HTML\nviewer. --seed N makes $random reproducible. Common library flags apply:\n-L name=path, --config, -I, -S, --lib-root (see `hdlsim help check`).",
        "check" => "Parse + lower + resolve the given files and print rustc-style\ndiagnostics (errors/warnings) without simulating. Fast way to validate a\nproject; --progress shows library indexing / unit resolution on stderr.\nLibrary resolution: ./hdlsim.ini (written by `hdlsim init`) or\n--config FILE, then -L name=path mappings, then --lib-root DIR trees (each\nsubdir of DIR becomes a library named after it; Xilinx unisim/unimacro/...\nare auto-detected via HDLSIM_XILINX_LIBS / XILINX_VIVADO / XILINX_HLS),\nthen -S DIR on-demand search roots, then `work = .` recursive scan of the\ncwd. Unit lookup is by design-unit name across all libraries (first match\nwins), so `use unisim.vcomponents.all;` works once unisim's dir is mapped.
Library-wide `use lib.all;` compiles every unit of library `lib`.
The default config is found by walking up from the cwd (like git),
so running from a project subdirectory still loads the root hdlsim.ini.\nIndexing is lazy: libraries are indexed by file name without parsing;\nfiles are parsed only when a unit name does not match its file name.",
        "wave" => "Render a .hwf (hdlsim binary) or .fst wave file to a standalone HTML\nviewer with -o out.html; without -o it prints the viewer path/serves it.",
        "ast" => "Parse one file and print its lossless CST as an s-expression — the\nreusable syntax tree the lowerer consumes. Useful when debugging parser\nissues; spans are file:line:col.",
        "init" => "Scan the project and write ./hdlsim.ini with a [Library] section.\nEach directory containing VHDL sources maps to a logical library named after\nthe directory's last component (one line per name; if several dirs share a\nname the shallowest path wins, so build-tree copies under vivado/... lose to\nthe real source dir). The project root is `work = .`. `library X;` clauses in\nsources are not used for naming (often stale). Vendor roots (--lib-root DIR,\nor HDLSIM_XILINX_LIBS / XILINX_VIVADO / XILINX_HLS `data/vhdl/src`) are\npersisted as absolute `name = path` lines so e.g. Xilinx unisim resolves.\nUse --force to overwrite, --progress to watch the scan.",
        "project" => "Emit a project file for an external tool: --format vivado writes a Tcl\nscript (add_files + vlog/vcom per library dir from hdlsim.ini/-L), --format\njson writes a machine-readable manifest of files, libraries and units.\n--top/--sim-top pick the elaboration/testbench tops.",
        "prove" => "Bounded model checking over the elaborated IR with the built-in CDCL SAT\nsolver: hdlsim prove --top dut [--bmc N] files... explores up to N steps and\nreports a counterexample trace on failure.",
        "debug" => "Interactive/one-shot debug of a simulation: --dump prints signal values at\nthe end, --watch sig prints every change of sig, --mem name dumps a memory,\n--all dumps everything, --max-time/--seed bound the run.",
        "bundle" => "Pack the project (sources, hdlsim.ini, include dirs) into one .tar\nreproducer for a bug report: hdlsim bundle --out repro.tar [--top tb]\n[files...]. Re-open elsewhere with `--bundle repro.tar` on any command.",
        "regress" => "Run a set of testbenches and emit JUnit XML (--junit out.xml):\n--list tests.txt reads tops from a file, --max-time bounds each, --coverage\nadds coverage info. Exit code reflects pass/fail.",
        "lsp" => "Language-server on stdio: initialize advertises full document sync,\ncompletion, hover and go-to-definition over the project libraries.",
        "fmt" => "Format Verilog/VHDL sources in place with -w, or print formatted output\nto stdout; --check exits non-zero if anything would change (CI gate).",
        "lint" => "Lint sources (style + common-mistake rules); --fix applies safe fixes,\nexit code 1 if any finding remains. Codes are explained by `hdlsim explain`.",
        "do" => "Run a mini-Tcl do-file (examine/force/run/quit/...) against a compiled\ndesign: hdlsim do script.do --top tb [files...].",
        "deps" => "List every file the top unit transitively depends on (libraries resolved\nvia hdlsim.ini/-L/--lib-root), in compile order — useful for build scripts.",
        "assist" => "Map a natural-language prompt to an hdlsim task (local router, --yes to\nskip confirmation): hdlsim assist \"why does tb.v fail\".",
        "explain" => "Explain a diagnostic/lint code (E0301, W0502, ...): what it means, an\nexample, and how to fix it. No argument lists all codes.",
        "gen" => "Generate a testbench skeleton from the design's top entity:\nhdlsim gen tb --top m [--lang verilog|vhdl] [-o out] [files...].",
        "analyze" => "Static heuristics on the compiled design: undriven (W0501) / unread\n(W0502) signals and related warnings per module.",
        "doc" => "Generate cross-linked HTML documentation from the sources into\nhtml-docs/ (--out DIR to change), linking entities, packages and instances.",
        "doctor" => "Check the environment: embedded stdlib present, hdlsim.ini readable and\nits dirs existing, -L dirs existing, include dirs, cache dir writable.\nExit code reflects the worst finding.",
        "completions" => "Print a shell completion script for bash/zsh/fish:\nhdlsim completions bash >> ~/.bashrc",
        "help" => "Show detailed help for a command: hdlsim help <command>.\nWith no argument, prints the top-level help.",
        "--help" => "Top-level help: command list plus the common flags.",
        _ => return None,
    })
}

/// Registry of all subcommands. Add one line per new command.
pub const COMMANDS: &[&dyn Command] = &[
    &Help,
    &HelpCmd,
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
    fn help_command_shows_per_command_docs() {
        assert_eq!(dispatch(&["help".to_string()]).unwrap(), 0);
        assert_eq!(
            dispatch(&["help".to_string(), "init".to_string()]).unwrap(),
            0
        );
        assert!(dispatch(&["help".to_string(), "frobnicate".to_string()]).is_err());
        // Every registered command has long-form docs.
        for c in COMMANDS {
            assert!(
                command_long_help(c.name()).is_some(),
                "missing long help for {}",
                c.name()
            );
        }
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
