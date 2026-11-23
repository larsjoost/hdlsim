//! Real-world corpus tests: third-party VHDL/Verilog from the internet (see
//! `corpus/README.md`) must lex + parse cleanly, and a full compile+lower
//! (`check`) must run to completion without panicking — diagnostics for
//! unsupported constructs are expected and must render rustc-style.
//!
//! Like a real project, each language directory is compiled as one library
//! (`hdlsim check -L neorv32=<dir> <dir>/*.vhd`): the directory is registered
//! under the logical library names its files declare, so cross-file units
//! (`use neorv32.neorv32_package.all;`) resolve on demand while compiling.

use hdl_lib::LibraryManager;
use hdl_syntax::language::Language;
use hdl_syntax::source::SourceDb;
use hdl_syntax::{verilog, vhdl};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

fn corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/corpus")
}

fn is_source_file(p: &Path) -> bool {
    matches!(
        p.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("v") | Some("sv") | Some("vhd") | Some("vhdl")
    )
}

fn dir_sources(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_file() && is_source_file(p))
        .collect();
    files.sort();
    files
}

/// All directories under `root` (recursive) that directly hold source files.
fn collect_group_dirs(root: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(root) {
        let mut entries: Vec<PathBuf> = rd.map(|e| e.unwrap().path()).collect();
        entries.sort();
        if entries.iter().any(|p| p.is_file() && is_source_file(p)) {
            out.push(root.to_path_buf());
        }
        for p in entries {
            if p.is_dir() {
                collect_group_dirs(&p, out);
            }
        }
    }
}

/// `(group, files)` groups — each group is compiled together as one library.
/// The OSVVM model tree (`vhdl-osvvm/<repo>/...`) is split per source directory:
/// each testbench directory is its own `work` library (its `TestCtrl` entity
/// must not collide with another testbench's), while each repo's `src` trees
/// are registered under the repo's logical library name.
fn corpus_groups() -> Vec<(String, Vec<PathBuf>)> {
    let mut out = Vec::new();
    for sub in ["verilog", "vhdl", "vhdl-vunit"] {
        out.push((sub.to_string(), dir_sources(&corpus_dir().join(sub))));
    }
    // OSVVM model repos are grouped per subproject (`vhdl-osvvm/<repo>/<sub>`
    // when the repo has subdirs, else `vhdl-osvvm/<repo>`): each subproject's
    // testbench entity (e.g. one `TestCtrl` per subproject) is then unique
    // within its group's `work` library. Duplicate-unit files (same unit name
    // in several testbench variants of a subproject) are excluded from the
    // corpus — see corpus/README.md.
    let osvvm_root = corpus_dir().join("vhdl-osvvm");
    if let Ok(rd) = std::fs::read_dir(&osvvm_root) {
        let mut repos: Vec<PathBuf> = rd
            .map(|e| e.unwrap().path())
            .filter(|p| p.is_dir())
            .collect();
        repos.sort();
        for repo in repos {
            let has_files = std::fs::read_dir(&repo)
                .map(|rd| rd.filter_map(|e| e.ok()).any(|e| is_source_file(&e.path())))
                .unwrap_or(false);
            let kids: Vec<PathBuf> = std::fs::read_dir(&repo)
                .map(|rd| {
                    rd.filter_map(|e| e.ok())
                        .map(|e| e.path())
                        .filter(|p| p.is_dir())
                        .collect()
                })
                .unwrap_or_default();
            let groups = if !kids.is_empty() && !has_files {
                kids
            } else {
                vec![repo.clone()]
            };
            // A `testcases` directory belongs to the sibling `testbench`
            // directory's `work` library: its files are architectures `of
            // TestCtrl`, and the `TestCtrl` entity lives in the testbench dir.
            let is_named = |g: &PathBuf, name: &str| {
                g.file_name()
                    .map_or(false, |n| n.to_string_lossy().eq_ignore_ascii_case(name))
            };
            let has_tb = groups.iter().any(|g| is_named(g, "testbench"));
            let tc_pos = groups.iter().position(|g| is_named(g, "testcases"));
            let mut group_roots: Vec<(PathBuf, Vec<PathBuf>)> = groups
                .iter()
                .map(|g| (g.clone(), vec![g.clone()]))
                .collect();
            if has_tb {
                if let Some(pos) = tc_pos {
                    let tc = groups[pos].clone();
                    let tb_pos = groups
                        .iter()
                        .position(|g| is_named(g, "testbench"))
                        .unwrap();
                    group_roots[tb_pos].1.push(tc);
                    group_roots.remove(pos);
                }
            }
            for (g, roots) in group_roots {
                let mut dirs = Vec::new();
                for r in &roots {
                    collect_group_dirs(r, &mut dirs);
                }
                let mut files = Vec::new();
                for d in dirs {
                    files.extend(dir_sources(&d));
                }
                files.sort();
                let rel = g
                    .strip_prefix(corpus_dir())
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((rel, files));
            }
        }
    }
    out
}

fn corpus_sources() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for (_, files) in corpus_groups() {
        out.extend(files);
    }
    assert!(out.len() >= 10, "corpus should hold the fetched files");
    out
}

fn rel_path(f: &Path) -> String {
    f.strip_prefix(corpus_dir())
        .unwrap()
        .to_string_lossy()
        .replace('\\', "/")
}

/// Logical library name of an OSVVM model repo dir (`vhdl-osvvm/<repo>`).
fn osvvm_repo_lib(repo: &str) -> Option<&'static str> {
    Some(match repo {
        "axi4" => "osvvm_axi4",
        "osvvm-common" => "osvvm_common",
        "uart" => "osvvm_uart",
        "avalonst" => "osvvm_avalonst",
        "wishbone" => "osvvm_wishbone",
        "ethernet" => "osvvm_ethernet",
        "dpram" => "osvvm_dpram",
        "spacewiremonitor" => "osvvm_spacewire",
        "spi" => "osvvm_spi",
        "videobus" => "osvvm_videobus",
        "cosim" => "osvvm_cosim",
        "cospcile" => "osvvm_cospcie",
        // The SpaceWire Light RTL core (freecores/spacewire_light, LGPL-2.1
        // for the GRLIB-free parts) — the DUT library of the SpaceWireMonitor
        // testbench.
        "spacewire-light" => "spacewire_light",
        _ => return None,
    })
}

/// One parse-cache directory shared by all group compiles in this test process
/// (the OSVVM tree is split into per-directory groups, and each group would
/// otherwise re-parse the shared `src` libraries).
fn shared_cache_dir() -> &'static PathBuf {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        std::env::temp_dir().join(format!("hdlsim-corpus-cache-{}", std::process::id()))
    })
}

/// Compile one group as a single library, registering the directory under the
/// logical library names its files declare in `library` clauses.
// ---- progress ----------------------------------------------------------------
//
// The built-in test harness prints one buffered dot per test, so a corpus
// run is opaque for its whole (multi-minute) duration. These notes go to
// stderr via `eprintln!` — stderr is unbuffered, so they appear live through
// pipes/tee with `RUST_TEST_NOCAPTURE=1 cargo test`.

/// Run-to-run stable name for a corpus file in progress notes.
fn progress_note(stage: &str, done: usize, total: usize, f: &Path) {
    if done % 25 == 0 || done == total {
        eprintln!("corpus: [{stage}] {done}/{total} — {}", rel_path(f));
    }
}

fn compile_group(sub: &str, files: &[PathBuf]) -> LibraryManager {
    eprintln!("corpus: group {sub}: compiling {} files…", files.len());
    let dir = corpus_dir().join(sub);
    let mut mgr = LibraryManager::new();
    if let Some(c) = hdl_lib::cache::Cache::open(shared_cache_dir().clone()) {
        mgr.enable_cache(c);
    }
    match sub {
        "vhdl" => {
            mgr.add_library_dir("neorv32", &dir);
            mgr.add_library_dir("unisim", &dir);
            mgr.add_library_dir("work", &dir);
        }
        // VUnit's own sources live in their own library so their `work.*`
        // references resolve within the group (not to same-named GHDL units).
        // Its test sources also reference `vunit_lib.*` (the logical name its
        // compile script uses) and `osvvm.*` (the OSVVM sources kept in `vhdl/`).
        "vhdl-vunit" => {
            mgr.add_library_dir("vunit", &dir);
            mgr.add_library_dir("vunit_lib", &dir);
            mgr.add_library_dir("work", &dir);
            mgr.add_library_dir("osvvm", &corpus_dir().join("vhdl"));
        }
        // OSVVM model tree: `work` is the group's own directory (per-testbench
        // `work.TestCtrl` etc. must not collide across testbenches); every
        // repo's `src` tree is registered under the repo's logical library
        // name (osvvm_axi4, osvvm_common, ...); the OSVVM core comes from the
        // `vhdl/` group.
        _ if sub.starts_with("vhdl-osvvm/") => {
            mgr.add_library_dir("work", &dir);
            let mut src_dirs = Vec::new();
            collect_group_dirs(&corpus_dir().join("vhdl-osvvm"), &mut src_dirs);
            for sd in &src_dirs {
                let rel = sd
                    .strip_prefix(corpus_dir().join("vhdl-osvvm"))
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                let mut comps = rel.split('/');
                let repo = comps.next().unwrap_or("");
                // Only `src` trees are library code, not testbench dirs.
                if !rel.split('/').any(|c| c.eq_ignore_ascii_case("src")) {
                    continue;
                }
                if let Some(lib) = osvvm_repo_lib(repo) {
                    mgr.add_library_dir(lib, sd);
                }
            }
            mgr.add_library_dir("osvvm", &corpus_dir().join("vhdl"));
        }
        _ => {
            mgr.add_library_dir("work", &dir);
        }
    }
    let mut design = hdl_ir::Design::default();
    mgr.compile_files(&mut design, files);
    mgr
}

/// Per-file status for a group compile: an error diagnostic counts against
/// the file its span points into; spanless errors count against every file.
/// Per-file status from an already-compiled group: an error diagnostic
/// counts against the file its span points into; spanless errors count
/// against every file.
fn statuses_from(mgr: &LibraryManager, files: &[PathBuf]) -> HashMap<String, &'static str> {
    let mut name_to_rel: HashMap<String, String> = HashMap::new();
    for f in files {
        name_to_rel.insert(f.display().to_string(), rel_path(f));
        if let Ok(canon) = std::fs::canonicalize(f) {
            name_to_rel.insert(canon.display().to_string(), rel_path(f));
        }
    }
    let mut err_files: HashSet<String> = HashSet::new();
    let mut spanless_error = false;
    for d in &mgr.diags().items {
        if d.severity != hdl_syntax::diagnostic::Severity::Error {
            continue;
        }
        match d.span {
            Some(sp) => match name_to_rel.get(mgr.db().file(sp.file).name()) {
                Some(rel) => {
                    err_files.insert(rel.clone());
                }
                None => spanless_error = true,
            },
            None => spanless_error = true,
        }
    }
    files
        .iter()
        .map(|f| {
            let rel = rel_path(f);
            let status = if spanless_error || err_files.contains(&rel) {
                "diags"
            } else {
                "clean"
            };
            (rel, status)
        })
        .collect()
}

fn parse_one(path: &Path) -> Vec<hdl_syntax::diagnostic::Diagnostic> {
    let text = std::fs::read_to_string(path).unwrap();
    let lang = Language::from_path(path).unwrap();
    let mut db = SourceDb::new();
    let id = db.add_file(path.display().to_string(), text).unwrap();
    match lang {
        Language::Vhdl => vhdl::parser::parse_file(id, db.file(id)).diags,
        Language::Verilog => {
            let mut macros = HashMap::new();
            let mut pdiags = Vec::new();
            let expanded =
                verilog::preproc::preprocess(id, db.file(id), &[], &mut macros, &mut pdiags);
            let id2 = db
                .add_file(path.display().to_string(), expanded)
                .unwrap_or(id);
            let mut parsed = verilog::parser::parse_file(id2, db.file(id2));
            parsed.diags.extend(pdiags);
            parsed.diags
        }
    }
}

/// Group compiles recurse deeply over the syntax tree (parse + lower); the
/// default 2 MiB test-thread stack overflows on the big framework files, so
/// the group-compile tests run on a thread with a large stack.
fn run_with_big_stack(f: impl FnOnce() + Send + 'static) {
    let handle = std::thread::Builder::new()
        .stack_size(512 * 1024 * 1024)
        .spawn(f)
        .unwrap();
    if let Err(e) = handle.join() {
        std::panic::resume_unwind(e);
    }
}

#[test]
fn corpus_files_lex_and_parse_without_errors() {
    let mut failures = Vec::new();
    let all = corpus_sources();
    let total = all.len();
    for (n, f) in all.iter().enumerate() {
        progress_note("parse", n + 1, total, f);
        let diags = parse_one(f);
        let errors: Vec<_> = diags
            .iter()
            .filter(|d| d.severity == hdl_syntax::diagnostic::Severity::Error)
            .collect();
        if !errors.is_empty() {
            failures.push(format!(
                "{}: {} parse error(s), first: {:?}",
                f.display(),
                errors.len(),
                errors[0]
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "parse failures:\n{}",
        failures.join("\n")
    );
}

/// One compile per group serves both the check-run assertions and the
/// per-file status snapshot. These were separate `#[test]`s that each
/// called `compile_group`, so the corpus — thousands of third-party
/// files — was lowered and analyzed twice per run for no added coverage;
/// the parse cache only skips lex/parse, not lowering. Merging halves
/// the corpus runtime.
#[test]
fn corpus_group_compiles_meet_expected_status_with_rustc_diags() {
    run_with_big_stack(|| {
        let expected_path = corpus_dir().join("expected.json");
        let groups = corpus_groups();
        let group_count = groups.len();
        // Groups are independent libraries in fresh managers, so they compile
        // concurrently on a worker pool — each compile is single-threaded and
        // CPU-bound, and the corpus has ~100+ groups. Each worker gets the
        // big stack the sequential loop needed (parse+lower recursion depth).
        let workers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .min(group_count.max(1));
        let next = std::sync::atomic::AtomicUsize::new(0);
        let checked = std::sync::atomic::AtomicUsize::new(0);
        let with_diags = std::sync::atomic::AtomicUsize::new(0);
        let actual = std::sync::Mutex::new(HashMap::new());
        std::thread::scope(|s| {
            for _ in 0..workers {
                std::thread::Builder::new()
                    .stack_size(512 * 1024 * 1024)
                    .spawn_scoped(s, || {
                        loop {
                            let i = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            if i >= group_count {
                                break;
                            }
                            let (sub, files) = &groups[i];
                            // One library compile per group, like
                            // `hdlsim check -L lib=dir dir/*`.
                            let t0 = std::time::Instant::now();
                            let mgr = compile_group(sub, files);
                            eprintln!(
                                "corpus: group {sub}: {} files in {:.1}s",
                                files.len(),
                                t0.elapsed().as_secs_f32()
                            );
                            checked.fetch_add(files.len(), std::sync::atomic::Ordering::SeqCst);
                            // rustc-style: every error line carries a stable
                            // `error[Exxxx]` code.
                            let rendered = mgr.diags().render_all(mgr.db());
                            if !mgr.diags().is_empty() {
                                with_diags.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                                for line in rendered.lines().filter(|l| l.starts_with("error")) {
                                    assert!(
                                        line.starts_with("error[") || line.starts_with("error:"),
                                        "{}: non-rustc-style line: {}",
                                        sub,
                                        line
                                    );
                                }
                            }
                            let mut map = actual.lock().unwrap();
                            for (rel, status) in statuses_from(&mgr, files) {
                                map.insert(rel, status.to_string());
                            }
                        }
                    })
                    .expect("spawn corpus worker");
            }
        });
        let actual = actual.into_inner().unwrap();
        let checked = checked.load(std::sync::atomic::Ordering::SeqCst);
        let with_diags = with_diags.load(std::sync::atomic::Ordering::SeqCst);
        // Sanity: the corpus is real code, so at least some files should produce
        // diagnostics for constructs outside the current subset (no panics either way).
        eprintln!(
            "corpus: {} files checked, {} group(s) with diagnostics",
            checked, with_diags
        );
        assert_eq!(checked, corpus_sources().len());
        if std::env::var_os("HDLSIM_UPDATE_CORPUS_EXPECTED").is_some() {
            let mut entries: Vec<(&String, &String)> = actual.iter().collect();
            entries.sort();
            let obj: serde_json::Value = serde_json::Value::Object(
                entries
                    .into_iter()
                    .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                    .collect(),
            );
            std::fs::write(
                &expected_path,
                format!("{}\n", serde_json::to_string_pretty(&obj).unwrap()),
            )
            .unwrap();
            eprintln!("corpus: wrote {}", expected_path.display());
            return;
        }
        let text = std::fs::read_to_string(&expected_path).unwrap_or_else(|e| {
            panic!(
                "read {}: {} (run with HDLSIM_UPDATE_CORPUS_EXPECTED=1 to create)",
                expected_path.display(),
                e
            )
        });
        let expected: HashMap<String, String> = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("parse {}: {}", expected_path.display(), e));
        for (rel, status) in &expected {
            assert!(
                corpus_dir().join(rel).exists(),
                "expected.json entry '{}' has no file on disk",
                rel
            );
            let actual_status = actual
                .get(rel)
                .unwrap_or_else(|| panic!("corpus file '{}' missing from this run", rel));
            assert!(
                rank(actual_status) >= rank(status),
                "{}: recorded status '{}' but actual '{}' — regression?",
                rel,
                status,
                actual_status
            );
        }
        for rel in actual.keys() {
            assert!(
            expected.contains_key(rel),
            "corpus file '{}' not in expected.json — add it (or snapshot with HDLSIM_UPDATE_CORPUS_EXPECTED=1)",
            rel
        );
        }
        let clean = actual.values().filter(|s| *s == "clean").count();
        eprintln!(
            "corpus: {}/{} files check-clean (expected.json)",
            clean,
            actual.len()
        );
    });
}

// ---- per-file expected status (corpus/expected.json) ------------------------
//
// Each corpus file has a recorded status in `corpus/expected.json`://   "clean" = compile+lower with zero *error* diagnostics (warnings ok)
//   "diags" = parses cleanly but still hits unsupported constructs
// Statuses are computed from a per-library group compile (see compile_group),
// attributing each error diagnostic to the file its span points into. The
// test asserts every file reaches at least its recorded status, so fixes
// only ever move files up (promote the entry in expected.json in the same PR
// that fixes the file). Regenerate/snapshot with HDLSIM_UPDATE_CORPUS_EXPECTED=1.

fn rank(status: &str) -> u8 {
    match status {
        "clean" => 2,
        "diags" => 1,
        _ => 0,
    }
}
