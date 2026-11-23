//! Regression test for the `**` (exponentiation) operator in VHDL:
//! constant-folded in constant initializers and width constraints, and
//! evaluated at runtime in processes.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

#[test]
fn pow_const_and_runtime() {
    let f = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vhdl_pow_tb.vhd");
    let out = hdlsim()
        .args(["run", "--top", "tb_pow"])
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
    assert!(stdout.contains("PASS: vhdl pow"), "{}", stdout);
    assert!(!stderr.contains("unsupported operator"), "{}", stderr);
}
