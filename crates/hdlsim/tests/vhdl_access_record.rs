//! Access types (implicit `.all` on field selects), nested record field
//! selects, bare physical unit names (`ns`), and the Xilinx implicit global
//! `GSR` — compile + simulate cleanly.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.env("HDLSIM_NO_CACHE", "1");
    c
}

#[test]
fn access_record_and_gsr_simulate() {
    let f =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vhdl_access_record_tb.vhd");
    let out = hdlsim()
        .args(["run", "--top", "tb_access"])
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
        stdout.contains("PASS: vhdl access/record/gsr"),
        "{}",
        stdout
    );
}
