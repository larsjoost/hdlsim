//! Regression test for VHDL `real` and the embedded `ieee.fixed_pkg` /
//! `ieee.float_pkg` subsets: real literals (incl. exponents), f64 arithmetic
//! and comparisons, `integer()`/`to_real()` conversion, and ufixed/sfixed/
//! float signals simulated as reals.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

#[test]
fn real_fixed_float_simulation() {
    let f = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vhdl_real_tb.vhd");
    let out = hdlsim()
        .args(["run", "--top", "tb_real"])
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
        stdout.contains("r=26.5 i=27 f=6.75 b=3 fl=-2.5"),
        "{}",
        stdout
    );
    assert!(stdout.contains("PASS: vhdl real/fixed/float"), "{}", stdout);
    assert!(!stdout.contains("FAIL"), "{}", stdout);
    // ieee.fixed_pkg / ieee.float_pkg must resolve without warnings.
    assert!(!stderr.contains("W0501"), "{}", stderr);
}

#[test]
fn check_accepts_real_fixed_float() {
    let f = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vhdl_real_tb.vhd");
    let out = hdlsim().args(["check"]).arg(&f).output().expect("runs");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "expected exit 0, got {:?}\n{}",
        out.status,
        stderr
    );
    assert!(!stderr.contains("error["), "{}", stderr);
}
