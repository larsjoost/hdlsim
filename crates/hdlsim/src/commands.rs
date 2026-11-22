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
mod ir;
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
        println!("       hdlsim help <command>   how to use one command\n");
        // Grouped by what you want to do, so the list answers "which
        // command do I use for X?" without reading every entry.
        for section in [
            "Simulate & debug:",
            "Check & clean up code:",
            "Generate & document:",
            "Inspect a design:",
            "Project & tooling:",
            "Help & bug reports:",
            "Other commands:",
        ] {
            let title = section.trim_end_matches(':');
            let cmds: Vec<_> = COMMANDS
                .iter()
                .filter(|c| section_of(c.name()) == title)
                .collect();
            if cmds.is_empty() {
                continue;
            }
            println!("{section}");
            for c in cmds {
                println!("  {:<12} {}", c.name(), c.about());
            }
            println!();
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
            "    --report \"msg\"             on failure, file a PUBLIC GitHub issue with a reproducer"
        );
        println!(
            "                                 bundle of your sources, uploaded as a gist and linked"
        );
        println!(
            "                                 (uses the gh CLI). A warning is printed up front because"
        );
        println!(
            "                                 the sources become public \u{2014} reduce the failure to a"
        );
        println!(
            "                                 minimal repro from the bundled examples/ first, or use"
        );
        println!(
            "                                 --report-dry-run to preview without uploading (also works"
        );
        println!("                                 without a message)");
        Ok(0)
    }
}

/// Which task group a command belongs to in the top-level help. Keep in
/// sync with `COMMANDS`: a command missing here falls under "Other
/// commands" (a unit test asserts every command is mapped).
fn section_of(name: &str) -> &'static str {
    match name {
        "run" | "debug" | "do" | "regress" | "wave" | "prove" => "Simulate & debug",
        "check" | "syntax" | "lint" | "analyze" | "fmt" => "Check & clean up code",
        "gen" | "doc" => "Generate & document",
        "ast" | "deps" | "ir" => "Inspect a design",
        "init" | "doctor" | "project" | "lsp" | "completions" => "Project & tooling",
        "help" | "--help" | "explain" | "assist" | "bundle" => "Help & bug reports",
        _ => "Other commands",
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
        // The one-liner is printed by clap's flag help below; here only the
        // usage documentation (what/when/how).
        if let Some(long) = command_long_help(cmd.name()) {
            println!("{long}\n");
        }
        println!(
            "\nflags (`hdlsim {} --help` shows the same list):",
            cmd.name()
        );
        cmd.run(&["--help".to_string()])
    }
}

/// Detailed long-form documentation per command, shown by `hdlsim help <cmd>`.
/// Usage only: what the command gets you, when to reach for it, how to call
/// it — not how it is implemented.
fn command_long_help(name: &str) -> Option<&'static str> {
    Some(match name {
        "run" => {
            "Compile the sources and simulate the testbench named by --top until it\n\
             stops or --max-time is reached. This is the everyday command:\n\
             \n\
             \x20   hdlsim run --top tb src/tb.v src/dut.v\n\
             \n\
             Exit codes: 0 = simulation finished clean; a failed assert, report\n\
             severity error, $error/$fatal or a compile error exits 1, so CI and\n\
             scripts can rely on it.\n\
             \n\
             Record waves:\n\
             \x20   --vcd f.vcd / --hwf f.hwf / --fst f.fst   write a wave file\n\
             \x20   --wave f.html                             standalone HTML viewer page\n\
             \x20   --live                                    open the live viewer in a browser (--port N)\n\
             \n\
             Reproducibility and limits:\n\
             \x20   --seed N       same seed = same random stimulus sequence\n\
             \x20   --max-time T   stop after T (10ns, 1us, 10ms); without it the run\n\
             \x20                  goes until no more events are scheduled\n\
             \n\
             Saved state:\n\
             \x20   --checkpoint f   save the simulator state when the run stops\n\
             \x20   --restore f      resume from a saved checkpoint\n\
             \n\
             Coverage:\n\
             \x20   --coverage            collect code coverage, print a report at the end\n\
             \x20   --coverage-out f      write the report to a file instead\n\
             \n\
             Where sources and libraries are found: see `hdlsim help check`."
        }
        "check" => {
            "Compile the files (with their dependencies) and print rustc-style\n\
             errors/warnings. No simulation. Use it to answer \"does this compile?\":\n\
             \n\
             \x20   hdlsim check src/dut.vhd src/tb.vhd\n\
             \n\
             Just edited one file and want a quick syntax look?\n\
             \n\
             \x20   hdlsim check --syntax-only file.vhd\n\
             \n\
             parses only the named files (no dependency compilation, no library\n\
             resolution) and reports lexical/syntax errors in milliseconds. A file\n\
             that references units it does not define still passes; compile-level\n\
             problems need the full check.\n\
             \n\
             check also elaborates every compiled top, so it reports missing\n\
             dependencies even for units nothing instantiates.\n\
             \n\
             Where sources and libraries are found (all commands share this):\n\
             \x20   1. --config FILE, else hdlsim.ini found by walking up from the cwd\n\
             \x20      (written by `hdlsim init`)\n\
             \x20   2. -L name=path logical library mappings\n\
             \x20   3. --lib-root DIR: each subdir becomes a library named after the dir\n\
             \x20      (Xilinx unisim/unimacro/... are auto-detected from\n\
             \x20      HDLSIM_XILINX_LIBS / XILINX_VIVADO / XILINX_HLS)\n\
             \x20   4. -S DIR on-demand search roots\n\
             \x20   5. `work` = the current directory tree\n\
             Units are found by design-unit name across all libraries, first match\n\
             wins. `use lib.all;` compiles every unit of that library."
        }
        "wave" => {
            "Turn a recorded wave file (.hwf or .fst) into a self-contained HTML\n\
             page you can open in any browser (default: next to the input,\n\
             waves.html):\n\
             \n\
             \x20   hdlsim wave waves.hwf\n\
             \x20   hdlsim wave waves.fst -o out.html\n\
             \n\
             Record waves during a run with `hdlsim run --vcd/--hwf/--fst/--wave`."
        }
        "ast" => {
            "Print how hdlsim parses one file: an s-expression tree whose nodes\n\
             carry file:line:col spans. Use it when a diagnostic points somewhere\n\
             odd, or when reporting a parser bug:\n\
             \n\
             \x20   hdlsim ast file.vhd"
        }
        "init" => {
            "Set up a project: scan the current tree and write hdlsim.ini, mapping\n\
             each source directory to a logical library. Run it once at the project\n\
             root (or in a fresh checkout):\n\
             \n\
             \x20   hdlsim init [--force] [--progress]\n\
             \n\
             Afterwards all commands find your libraries through hdlsim.ini."
        }
        "project" => {
            "Export the file list for other tools:\n\
             \n\
             \x20   --format vivado   Tcl script (add_files + vcom/vlog per library dir)\n\
             \x20   --format json     machine-readable manifest of files, libraries, units\n\
             \n\
             \x20   hdlsim project --format vivado --top t --sim-top tb [--out f.tcl] [files]"
        }
        "prove" => {
            "Bounded model check: does the DUT stay within its assertions? Explores\n\
             every reachable state up to --bmc N steps and either proves the bound\n\
             or prints a counterexample trace you can open with `hdlsim wave`:\n\
             \n\
             \x20   hdlsim prove --top dut [--bmc N] [files]"
        }
        "debug" => {
            "Run a simulation with inspection output instead of just a transcript:\n\
             \n\
             \x20   --dump        print every signal's final value (width, memory, init)\n\
             \x20   --watch sig   print every change of one signal\n\
             \x20   --driver sig  trace what drives one signal\n\
             \x20   --mem name    dump one memory\n\
             \x20   --all         dump everything\n\
             \x20   --max-time T / --seed N   as in run\n\
             \n\
             Use it when a run misbehaves: dump the elaborated design (widths,\n\
             memories, instance tree), watch a suspect signal change by change, or\n\
             inspect a memory at the end. Diagnostics and exit codes are the same\n\
             as run."
        }
        "bundle" => {
            "Pack the sources of a failing design into one .tar others can rerun:\n\
             \n\
             \x20   hdlsim bundle --out repro.tar [--top tb] [files]\n\
             \n\
             Open it anywhere with `--bundle repro.tar` on any command. Prefer a\n\
             minimal reproducer from the bundled examples/ before attaching it to\n\
             an issue — the tar contains your sources."
        }
        "regress" => {
            "Run several testbenches in one go and summarize for CI:\n\
             \n\
             \x20   hdlsim regress tb_a tb_b          the named tops\n\
             \x20   hdlsim regress --list tests.txt   tops from a file (one per line,\n\
             \x20                                     `name=top` renames the case)\n\
             \x20   --junit out.xml                   JUnit XML report for CI systems\n\
             \x20   --max-time T                      bound each testbench\n\
             \x20   --coverage                        per-test coverage, folded into the report\n\
             \n\
             Exit code is non-zero when any testbench fails (assertion/report\n\
             errors, $error/$fatal — same rule as run)."
        }
        "lsp" => {
            "Run the language server for editor integration (stdio): live parse\n\
             diagnostics as you type, completion, hover, go-to-definition over the\n\
             project libraries. Editors start it themselves (see README); you\n\
             rarely run this by hand."
        }
        "fmt" => {
            "Format sources:\n\
             \n\
             \x20   hdlsim fmt file.v           print the formatted result\n\
             \x20   hdlsim fmt -w file.v        rewrite the file in place\n\
             \x20   hdlsim fmt --check file.v   exit 1 if it would change (CI gate)"
        }
        "lint" => {
            "Flag common mistakes and style issues with stable codes:\n\
             \n\
             \x20   hdlsim lint [files]         report findings\n\
             \x20   hdlsim lint --fix [files]   apply the safe fixes automatically\n\
             \n\
             Exit 1 when any finding remains, so it gates CI. What a code means:\n\
             `hdlsim explain W0502`."
        }
        "do" => {
            "Run a ModelSim-style .do script against a design compiled by\n\
             hdlsim. Supported simulator commands: run [time|-all], restart,\n\
             back [N] (reverse debug — undo the last N run steps), vcd\n\
             dumpfile PATH, examine NAME, seed N, quit:\n\
             \n\
             \x20   hdlsim do script.do --top tb [files]"
        }
        "deps" => {
            "List every file the top unit needs, in compile order — the list you\n\
             would pass to a compile step or a build script:\n\
             \n\
             \x20   hdlsim deps --top tb [files]"
        }
        "assist" => {
            "Describe what you want in plain English; hdlsim proposes the command\n\
             and, with --yes, runs it. Routing is local — your prompt does not\n\
             leave the machine:\n\
             \n\
             \x20   hdlsim assist \"why does tb.v fail\"\n\
             \x20   hdlsim assist --yes \"dump all signals of tb\""
        }
        "explain" => {
            "Explain an error, warning or lint code: what it means, an example,\n\
             and how to fix it.\n\
             \n\
             \x20   hdlsim explain E0301     one code\n\
             \x20   hdlsim explain           list every code"
        }
        "gen" => {
            "Write a testbench skeleton for a module: ports copied, one stimulus\n\
             process to fill in.\n\
             \n\
             \x20   hdlsim gen tb --top m [files]                print it\n\
             \x20   hdlsim gen tb --top m -o tb_m.vhd [files]    write it\n\
             \x20   --lang verilog|vhdl                          output language"
        }
        "analyze" => {
            "Report wiring problems in a compiled design: signals that are never\n\
             driven (W0501), never read (W0502), and related warnings. Use it\n\
             after wiring a new module to catch dangling logic:\n\
             \n\
             \x20   hdlsim analyze [files]"
        }
        "doc" => {
            "Generate cross-linked HTML documentation (entities, packages,\n\
             hierarchy) into html-docs/ (--out DIR changes it):\n\
             \n\
             \x20   hdlsim doc [files]"
        }
        "doctor" => {
            "Verify the installation works in this project: stdlib present,\n\
             hdlsim.ini readable, library and include directories existing, cache\n\
             writable. Run it when units or packages cannot be found. Exit code\n\
             reflects the worst finding:\n\
             \n\
             \x20   hdlsim doctor"
        }
        "completions" => {
            "Print a shell completion script:\n\
             \n\
             \x20   hdlsim completions bash >> ~/.bashrc\n\
             \n\
             Supported: bash, zsh, fish."
        }
        "ir" => {
            "Dump the elaborated IR of one top as pretty JSON:\n\
             \n\
             \x20   hdlsim ir --top tb file.vhd [--out ir.json]\n\
             \n\
             Signals, processes, continuous assigns, and function bodies —\n\
             the same structures and indices the simulation traces report,\n\
             so a trace like Sig(4635) can be looked up in the dump."
        }
        "syntax" => {
            "Parse-only check of the named files — short for check --syntax-only:\n\
             \n\
             \x20   hdlsim syntax file.vhd [more.vhd ...]\n\
             \n\
             Reports lexical/syntax errors per file; no libraries, no\n\
             compilation, no elaboration. Same output and exit codes as\n\
             `check --syntax-only`."
        }
        "help" => {
            "Show detailed help for a command: hdlsim help <command>.\n\
             With no argument, prints the top-level help."
        }
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
    &check::SyntaxCmd,
    &ast::Ast,
    &ir::Ir,
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

/// Resolve the command name the user typed to a registered command: exact
/// match first, then a unique prefix (`syn` → syntax), then a single close
/// typo (`chekc` → check). Ambiguous or unknown names are an error that
/// lists the candidates — a guess never silently runs the wrong subcommand.
fn resolve_command(typed: &str) -> anyhow::Result<&'static dyn Command> {
    if let Some(c) = COMMANDS.iter().find(|c| c.name() == typed) {
        return Ok(*c);
    }
    let prefixed: Vec<&dyn Command> = COMMANDS
        .iter()
        .copied()
        .filter(|c| c.name().starts_with(typed))
        .collect();
    if prefixed.len() == 1 {
        let c = prefixed[0];
        eprintln!(
            "note: no command '{typed}' — running '{}' ({})",
            c.name(),
            c.about()
        );
        return Ok(c);
    }
    if prefixed.len() > 1 {
        let mut names: Vec<&str> = prefixed.iter().map(|c| c.name()).collect();
        names.sort_unstable();
        anyhow::bail!(
            "command '{}' is ambiguous: {}; use more characters or `hdlsim --help`",
            typed,
            names.join(", ")
        );
    }
    // No prefix: accept a single close typo as the intended command.
    let max_d = if typed.len() < 4 { 1 } else { 2 };
    let close: Vec<&dyn Command> = COMMANDS
        .iter()
        .copied()
        .filter(|c| edit_distance(c.name(), typed) <= max_d)
        .collect();
    if close.len() == 1 {
        let c = close[0];
        eprintln!(
            "note: no command '{typed}' — running closest match '{}' ({})",
            c.name(),
            c.about()
        );
        return Ok(c);
    }
    if close.len() > 1 {
        let mut names: Vec<&str> = close.iter().map(|c| c.name()).collect();
        names.sort_unstable();
        anyhow::bail!(
            "no command '{}' — closest matches: {}; see `hdlsim --help`",
            typed,
            names.join(", ")
        );
    }
    anyhow::bail!("unknown command '{}'; see `hdlsim --help`", typed)
}

/// Levenshtein distance (insert/delete/substitute), stdlib-only.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur: Vec<usize> = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let sub = prev[j - 1] + usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(sub);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

pub fn dispatch(args: &[String]) -> anyhow::Result<i32> {
    match args.first() {
        None => COMMANDS[0].run(&[]),
        Some(name) => {
            let cmd = resolve_command(name)?;
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
            // Grouped top-level help: a command without an explicit section
            // would land under "Other commands", i.e. the mapping was
            // forgotten when the command was registered.
            assert_ne!(
                section_of(c.name()),
                "Other commands",
                "command '{}' has no section in section_of()",
                c.name()
            );
        }
    }

    #[test]
    fn unknown_command_is_an_error() {
        assert!(dispatch(&["frobnicate".to_string()]).is_err());
    }

    #[test]
    fn partial_command_resolves_to_unique_prefix() {
        // `wav` → wave; the routed command prints its own help, exit 0.
        assert_eq!(
            dispatch(&["wav".to_string(), "--help".to_string()]).unwrap(),
            0
        );
    }

    #[test]
    fn ambiguous_prefix_lists_candidates() {
        let err = dispatch(&["d".to_string()]).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("ambiguous"), "{msg}");
        assert!(msg.contains("deps") && msg.contains("doctor"), "{msg}");
    }

    #[test]
    fn single_typo_resolves_to_closest_command() {
        // `chekc` is edit distance 2 from `check` and from nothing else.
        assert_eq!(
            dispatch(&["chekc".to_string(), "--help".to_string()]).unwrap(),
            0
        );
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
