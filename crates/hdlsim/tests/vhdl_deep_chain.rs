//! Regression tests for deep selected-name chains: record field → array
//! index → record field (…), rooted at constants and at record-typed
//! function formals, with `others` choices in nested aggregates.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/{}", name))
}

#[test]
fn chain_with_mid_index_folds_from_const_and_formal() {
    let f = fixture("vhdl_deep_chain_tb.vhd");
    let out = hdlsim()
        .args(["run", "--top", "deep_chain_tb"])
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
    // Const root: the two-level select (control) and the three-level chain
    // with the index in the middle must both fold — no X, no E0401.
    assert!(stdout.contains("C_MID_TAG=7"), "{}", stdout);
    assert!(stdout.contains("C_LEAF_TAG=0"), "{}", stdout);
    // Formal root: the same chain inside package-body functions.
    assert!(stdout.contains("C_FN_TAG=7"), "{}", stdout);
    assert!(stdout.contains("C_FN_NOTE=70"), "{}", stdout);
    assert!(!stderr.contains("E0401"), "{}", stderr);
}
