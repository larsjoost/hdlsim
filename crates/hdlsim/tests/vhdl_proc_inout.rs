//! Regression tests for procedure `variable inout`/`out` copy-back: a
//! procedure's final formal values must reach the caller's variable on
//! return (LRM 1076 §4.3) — previously every write inside the procedure
//! was dropped, at process level and in nested calls alike.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

/// Scalar, `out`-mode, bit, array-element, and nested-frame inout actuals
/// all observe the procedure's writes after the call returns.
#[test]
fn procedure_inout_copies_back_to_caller_variable() {
    let f = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vhdl_proc_inout_tb.vhd");
    let out = hdlsim()
        .args(["run", "--top", "proc_inout_tb"])
        .arg(&f)
        .output()
        .expect("runs");
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "run failed:\n{all}");
    assert!(all.contains("inout_tb: PASS"), "{all}");
    assert!(!all.contains("FAIL"), "{all}");
}
