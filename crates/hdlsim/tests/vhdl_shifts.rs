//! Regression test for VHDL shift/rotate operators: `sll`/`srl`/`sla`/`sra`/
//! `rol`/`ror` and the std_logic_arith function forms `shift_left` /
//! `shift_right` / `rotate_left` / `rotate_right`. (Operators were mapped but
//! rol/ror incorrectly to plain shifts; the function forms were unsupported.)

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.env("HDLSIM_NO_CACHE", "1");
    c
}

#[test]
fn shifts_and_rotates_simulate() {
    let f = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vhdl_shifts_tb.vhd");
    let out = hdlsim()
        .args(["run", "--top", "tb_shifts"])
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
    assert!(stdout.contains("PASS: vhdl shifts"), "{}", stdout);
    assert!(!stderr.contains("unsupported operator"), "{}", stderr);
}
