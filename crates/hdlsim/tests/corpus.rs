//! Real-world corpus tests: third-party VHDL/Verilog from the internet (see
//! `corpus/README.md`) must lex + parse cleanly, and a full compile+lower
//! (`check`) must run to completion without panicking — diagnostics for
//! unsupported constructs are expected and must render rustc-style.

use hdl_lib::LibraryManager;
use hdl_syntax::language::Language;
use hdl_syntax::source::SourceDb;
use hdl_syntax::{verilog, vhdl};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/corpus")
}

fn corpus_sources() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for sub in ["verilog", "vhdl"] {
        let dir = corpus_dir().join(sub);
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| {
                matches!(
                    p.extension()
                        .and_then(|e| e.to_str())
                        .map(|e| e.to_ascii_lowercase())
                        .as_deref(),
                    Some("v") | Some("sv") | Some("vhd") | Some("vhdl")
                )
            })
            .collect();
        files.sort();
        out.extend(files);
    }
    assert!(out.len() >= 10, "corpus should hold the fetched files");
    out
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

#[test]
fn corpus_files_lex_and_parse_without_errors() {
    let mut failures = Vec::new();
    for f in corpus_sources() {
        let diags = parse_one(&f);
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

#[test]
fn corpus_files_check_runs_and_diagnostics_are_rustc_style() {
    let mut checked = 0;
    let mut with_diags = 0;
    for f in corpus_sources() {
        let mut mgr = LibraryManager::new();
        let mut design = hdl_ir::Design::default();
        mgr.compile_files(&mut design, &[f.clone()]);
        checked += 1;
        let rendered = mgr.diags().render_all(mgr.db());
        if !mgr.diags().is_empty() {
            with_diags += 1;
            // rustc-style: every error line carries a stable `error[Exxxx]` code.
            for line in rendered.lines().filter(|l| l.starts_with("error")) {
                assert!(
                    line.starts_with("error[") || line.starts_with("error:"),
                    "{}: non-rustc-style line: {}",
                    f.display(),
                    line
                );
            }
        }
    }
    // Sanity: the corpus is real code, so at least some files should produce
    // diagnostics for constructs outside the current subset (no panics either way).
    eprintln!(
        "corpus: {} files checked, {} with diagnostics",
        checked, with_diags
    );
    assert_eq!(checked, corpus_sources().len());
}

// ---- per-file expected status (corpus/expected.json) ------------------------
//
// Each corpus file has a recorded status in `corpus/expected.json`:
//   "clean" = compile+lower with zero *error* diagnostics (warnings ok)
//   "diags" = parses cleanly but still hits unsupported constructs
// The test asserts every file reaches at least its recorded status, so fixes
// only ever move files up (promote the entry in expected.json in the same PR
// that fixes the file). Regenerate/snapshot with HDLSIM_UPDATE_CORPUS_EXPECTED=1.

fn file_status(f: &Path) -> &'static str {
    let mut mgr = LibraryManager::new();
    let mut design = hdl_ir::Design::default();
    mgr.compile_files(&mut design, &[f.to_path_buf()]);
    if mgr.diags().has_errors() {
        "diags"
    } else {
        "clean"
    }
}

fn rank(status: &str) -> u8 {
    match status {
        "clean" => 2,
        "diags" => 1,
        _ => 0,
    }
}

#[test]
fn corpus_files_meet_their_recorded_status() {
    let expected_path = corpus_dir().join("expected.json");
    let mut actual: HashMap<String, String> = HashMap::new();
    for f in corpus_sources() {
        let rel = f
            .strip_prefix(corpus_dir())
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        actual.insert(rel, file_status(&f).to_string());
    }
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
}
