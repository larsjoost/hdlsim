//! Regression test for arrays of records: `arr(i).field` selects (read and
//! write), comma-separated record fields, record-typed ports with field
//! slices, and dotted names in sensitivity lists.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

#[test]
fn arrays_of_records_runtime() {
    let f = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vhdl_arr_rec_tb.vhd");
    let out = hdlsim()
        .args(["run", "--top", "tb_arr_rec"])
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
        stdout.contains("PASS: vhdl arrays of records"),
        "{}{}",
        stdout,
        stderr
    );
    assert!(!stdout.contains("assertion failed"), "{}", stdout);
}

#[test]
fn arrays_of_records_check() {
    let f = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vhdl_arr_rec_tb.vhd");
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
