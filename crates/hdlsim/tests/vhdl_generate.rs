//! Regression test for for/if/case-generate and block statements: generate
//! bodies must be unrolled/selected at lower time, not silently ignored.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

#[test]
fn generate_and_block_simulate() {
    let f = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vhdl_generate_tb.vhd");
    let out = hdlsim()
        .args(["run", "--top", "tb_generate"])
        .arg(&f)
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
    assert!(stdout.contains("PASS: vhdl generate/block"), "{}", stdout);
    assert!(!stderr.contains("not supported"), "{}", stderr);
}

#[test]
fn generate_file_checks_clean() {
    let f = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vhdl_generate_tb.vhd");
    let out = hdlsim().arg("check").arg(&f).output().expect("runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "expected exit 0, got {:?}\n{}{}",
        out.status,
        stdout,
        stderr
    );
    assert!(stdout.contains("0 diagnostics"), "{}", stdout);
}
