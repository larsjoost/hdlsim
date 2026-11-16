//! Regression tests for record types: `type t is record … end record;` lowers
//! to a flat vector, `r.field` selects a field slice (read and write), and
//! named aggregates `(field => value, …)` are ordered by declaration.
//! (Named associations previously failed to *parse*.)

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.env("HDLSIM_NO_CACHE", "1");
    c
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/{}", name))
}

#[test]
fn record_fields_check_and_simulate() {
    let f = fixture("vhdl_record_tb.vhd");
    let out = hdlsim()
        .args(["run", "--top", "tb_record"])
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
    assert!(stdout.contains("PASS: vhdl record fields"), "{}", stdout);
    assert!(!stderr.contains("E0401"), "{}", stderr);
}
