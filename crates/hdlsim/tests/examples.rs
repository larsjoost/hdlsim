//! Feature examples: every file in `examples/features/` is a self-checking
//! design (Verilog/SV `module tb` printing `PASS`, or VHDL `entity tb`
//! reporting `PASS`) that exercises one language feature area. This test runs
//! each through the CLI and requires the PASS line and a clean exit.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

fn features_dir() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/features"
    ))
}

fn feature_files() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(features_dir())
        .expect("examples/features exists")
        .map(|e| e.unwrap().path())
        .filter(|p| {
            matches!(
                p.extension().and_then(|e| e.to_str()),
                Some("sv") | Some("v") | Some("vhd") | Some("vhdl")
            )
        })
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no feature examples found");
    files
}

#[test]
fn every_feature_example_prints_pass() {
    let mut failed = Vec::new();
    for f in feature_files() {
        let out = hdlsim()
            .args(["run", "--top", "tb", "--max-time", "1ms"])
            .arg(&f)
            .output()
            .expect("runs hdlsim");
        let text = String::from_utf8_lossy(&out.stdout).to_string()
            + &String::from_utf8_lossy(&out.stderr);
        let pass = text
            .lines()
            .any(|l| l.trim() == "PASS" || l.trim() == ": PASS");
        if !pass || !out.status.success() {
            failed.push(format!(
                "{}: status={:?} pass={}\n{}",
                f.display(),
                out.status.code(),
                pass,
                text
            ));
        }
    }
    assert!(
        failed.is_empty(),
        "failing examples:\n{}",
        failed.join("\n---\n")
    );
}

#[test]
fn feature_examples_cover_both_languages() {
    let files = feature_files();
    assert!(
        files
            .iter()
            .any(|f| f.extension().and_then(|e| e.to_str()) == Some("sv")),
        "no SystemVerilog examples"
    );
    assert!(
        files
            .iter()
            .any(|f| f.extension().and_then(|e| e.to_str()) == Some("vhd")),
        "no VHDL examples"
    );
}
