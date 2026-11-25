//! Verifies the coding-standards enforcement stays wired (see AGENTS.md).
//!
//! Two failure modes, both machine-checked here:
//! 1. Wiring — the workspace lint baseline, the clippy thresholds, the
//!    per-crate opt-ins, and the CI gate lines all exist. A config edit,
//!    or a new crate without `lints.workspace = true`, fails here fast.
//! 2. Mechanism — a probe crate whose manifest `[lints]` mirror the
//!    baseline must FAIL `cargo clippy -- -D warnings`, naming every
//!    standard's lint. A green gate alone proves nothing: clippy silently
//!    ignores inert lints (excessive_nesting is nightly-only and passes
//!    without any warning on stable), so a standard is only enforced if
//!    its violations actually fail the build.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Workspace root, resolved from the compile-time crate path so the test
/// does not depend on the shell's working directory.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("crates/hdlsim sits two levels under the workspace root")
}

fn read(root: &Path, rel: &str) -> String {
    fs::read_to_string(root.join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

/// Maps each `[section]` header to its body (lines joined), with content
/// before the first header under "[root]". Textual on purpose: the vetted
/// dependency set has no TOML crate, and the assertions below are
/// deliberately strict — this test exists to fail when wiring is touched.
fn toml_sections(text: &str) -> Vec<(String, String)> {
    let mut sections: Vec<(String, Vec<&str>)> = vec![(String::from("[root]"), Vec::new())];
    for line in text.lines() {
        if line.starts_with('[') {
            sections.push((line.trim().to_string(), Vec::new()));
        } else if let Some(last) = sections.last_mut() {
            last.1.push(line);
        }
    }
    sections
        .into_iter()
        .map(|(name, body)| (name, body.join("\n")))
        .collect()
}

fn section<'a>(sections: &'a [(String, String)], name: &str) -> &'a str {
    sections
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, body)| body.as_str())
        .unwrap_or_else(|| panic!("section {name} missing"))
}

#[test]
fn coding_standards_configuration_is_wired() {
    let root = workspace_root();

    let sections = toml_sections(&read(&root, "Cargo.toml"));
    let rust_lints = section(&sections, "[workspace.lints.rust]");
    assert!(
        rust_lints.contains("unsafe_code = \"deny\""),
        "no-unsafe baseline missing: {rust_lints}"
    );
    let clippy_lints = section(&sections, "[workspace.lints.clippy]");
    for lint in [
        "dbg_macro",
        "todo",
        "unimplemented",
        "undocumented_unsafe_blocks",
    ] {
        assert!(
            clippy_lints.contains(&format!("{lint} = \"warn\"")),
            "baseline lint {lint} missing: {clippy_lints}"
        );
    }

    let clippy_toml = read(&root, "clippy.toml");
    assert!(
        clippy_toml.contains("too-many-lines-threshold = 50"),
        "function-length split line missing: {clippy_toml}"
    );
    assert!(
        clippy_toml.contains("cognitive-complexity-threshold = 30"),
        "complexity split line missing: {clippy_toml}"
    );

    // Every workspace crate must opt in — including crates added later.
    let crates = fs::read_dir(root.join("crates")).expect("crates dir exists");
    for entry in crates {
        let path = entry.expect("crate entry readable").path();
        if !path.join("Cargo.toml").is_file() {
            continue;
        }
        let name = path.file_name().expect("dir has a name").to_string_lossy();
        let sections = toml_sections(&read(&root, &format!("crates/{name}/Cargo.toml")));
        let lints = section(&sections, "[lints]");
        assert!(
            lints.contains("workspace = true"),
            "crate {name} does not opt in to the workspace lints: {lints}"
        );
    }

    let ci = read(&root, ".github/workflows/ci.yml");
    assert!(
        ci.contains("cargo clippy --all-targets -- -D warnings"),
        "the all-targets zero-findings gate is missing"
    );
    assert!(
        ci.contains("cargo clippy --workspace --lib -- -D clippy::unwrap_used -D clippy::panic"),
        "the library crash-safety gate is missing"
    );
    assert!(
        ci.contains("clippy::too_many_lines") && ci.contains("clippy::cognitive_complexity"),
        "the report-only shape audits are missing"
    );
}

/// Manifest mirroring `[workspace.lints]` exactly — the mechanism under
/// test is package `[lints]`, which is how cargo materializes the
/// workspace baseline for every crate that opts in.
const PROBE_MANIFEST: &str = r#"
[package]
name = "standards_probe"
version = "0.1.0"
edition = "2021"

[lints.rust]
unsafe_code = "deny"

[lints.clippy]
dbg_macro = "warn"
todo = "warn"
unimplemented = "warn"
undocumented_unsafe_blocks = "warn"
"#;

/// One violation per standard, in otherwise-warning-free code (pub fn:
/// no dead_code; x used: no unused_variables; the unsafe block carries
/// no SAFETY comment, which is exactly what undocumented_unsafe_blocks
/// looks for).
const PROBE_VIOLATIONS: &str = r#"
pub fn probe(x: i32) -> i32 {
    dbg!(x);
    if x > 5 {
        todo!("probe");
    }
    if x < 0 {
        unimplemented!("probe");
    }
    unsafe {
        let _ = std::ptr::read_volatile(&x as *const i32);
    }
    panic!("probe");
}
"#;

#[test]
fn clippy_gate_fails_on_standards_violations_and_names_the_lints() {
    // Fresh temp dir per run; outside the workspace so cargo cannot walk
    // up into our Cargo.toml, and with its own target dir so the nested
    // cargo never touches the build lock the outer `cargo test` holds.
    let probe = std::env::temp_dir().join(format!("hdlsim-standards-probe-{}", std::process::id()));
    fs::create_dir_all(probe.join("src")).expect("create probe crate dir");
    fs::write(probe.join("Cargo.toml"), PROBE_MANIFEST).expect("write probe manifest");
    fs::write(probe.join("src/lib.rs"), PROBE_VIOLATIONS).expect("write probe source");

    // Isolate the probe build from the outer cargo: no inherited target
    // dir, rustflags, or build-script env may leak in. The toolchain
    // selection (RUSTUP_TOOLCHAIN, PATH) is deliberately kept.
    let mut cmd = Command::new("cargo");
    cmd.arg("clippy")
        .arg("--manifest-path")
        .arg(probe.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(probe.join("target"))
        .args(["--", "-D", "warnings", "-D", "clippy::panic"]);
    let inherited: Vec<_> = std::env::vars_os()
        .filter(|(k, _)| {
            let k = k.to_string_lossy();
            !(k.starts_with("CARGO_") || k == "RUSTFLAGS" || k == "RUSTC_WRAPPER")
        })
        .collect();
    cmd.env_clear();
    for (k, v) in inherited {
        cmd.env(k, v);
    }

    let out = cmd.output().expect("run cargo clippy on the probe crate");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        !out.status.success(),
        "standards violations must fail the gate: {text}"
    );
    // The clippy help anchors name the lint per finding; the rustc deny
    // for unsafe_code reports "usage of an `unsafe` block".
    for marker in [
        "index.html#dbg_macro",
        "index.html#todo",
        "index.html#unimplemented",
        "index.html#undocumented_unsafe_blocks",
        "index.html#panic",
        "usage of an `unsafe` block",
    ] {
        assert!(text.contains(marker), "gate must name {marker}: {text}");
    }

    let _ = fs::remove_dir_all(&probe);
}
