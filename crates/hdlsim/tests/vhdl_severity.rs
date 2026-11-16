//! Regression test for the std.standard `severity_level` literals
//! (`note`/`warning`/`error`/`failure`), e.g. `MsgSeverity => ERROR` in
//! Xilinx UNISIM VHDL primitives (corpus `unisim_ODDR.vhd`).

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.env("HDLSIM_NO_CACHE", "1");
    c
}

#[test]
fn severity_literals_resolve_and_simulate() {
    let f = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vhdl_severity_tb.vhd");
    let out = hdlsim()
        .args(["run", "--top", "tb_severity"])
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
        stdout.contains("PASS: vhdl severity literals"),
        "{}",
        stdout
    );
    assert!(!stderr.contains("unknown identifier"), "{}", stderr);
}
