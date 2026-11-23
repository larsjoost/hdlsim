//! Regression test for `sig'last_value` and `sig'stable` attributes.
//! 'last_value previously lowered to X (empty via 'image), because the
//! `t'image(sig'last_value)` call excluded attribute arguments.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

#[test]
fn last_value_and_stable_simulate() {
    let f = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vhdl_lastval_tb.vhd");
    let out = hdlsim()
        .args(["run", "--top", "tb_lastval"])
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
    assert!(
        stdout.contains("PASS: vhdl last_value/stable"),
        "{}",
        stdout
    );
    assert!(!stderr.contains("not supported"), "{}", stderr);
}
