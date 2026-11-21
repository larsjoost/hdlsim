//! Regression tests for cross-file generic binding: an architecture lowered
//! from a different file than its entity needs the entity's type/constant
//! snapshot restored so record-typed generic field selects resolve, and an
//! instantiation's generic map must re-lower the sibling architecture file
//! as a variant instead of leaving the variant an empty shell.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.env("HDLSIM_NO_CACHE", "1");
    c
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Compile and run the given fixtures in order; asserts exit 0 and returns
/// the transcript.
fn run_ok(top: &str, files: &[&str]) -> String {
    let out = hdlsim()
        .args(["run", "--top", top])
        .args(files.iter().map(|f| fixture(f)))
        .output()
        .expect("runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "expected exit 0, got {:?}\n{}{}",
        out.status,
        stdout,
        stderr
    );
    stdout.to_string()
}

#[test]
fn cross_file_record_generic_resolves() {
    let out = run_ok(
        "tb_cross_generic",
        &[
            "vhdl_cross_generic_pkg.vhd",
            "vhdl_cross_generic_ent.vhd",
            "vhdl_cross_generic_arch.vhd",
            "vhdl_cross_generic_tb.vhd",
        ],
    );
    assert!(
        out.contains("PASS: cross-file record generic field selects resolve"),
        "{}",
        out
    );
}

#[test]
fn variant_relowers_sibling_architecture() {
    let out = run_ok(
        "tb_variant_arch",
        &[
            "vhdl_variant_arch_ent.vhd",
            "vhdl_variant_arch_rtl.vhd",
            "vhdl_variant_arch_tb.vhd",
        ],
    );
    assert!(
        out.contains("PASS: generic-map variant re-lowered with sibling architecture"),
        "{}",
        out
    );
}
