//! Regression test for VHDL-2008 `force`/`release` signal assignments
//! (`s <= force '1';`, `s <= release '0';`, and per-element force in a
//! multi-element waveform) — used by OSVVM's CoSim testbench
//! (`gIntReq(0) <= force '1';` in TbAb_CoSim.vhd). `force`/`release` are
//! reserved words: after them a tick must lex as a char literal, not an
//! attribute tick.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.env("HDLSIM_NO_CACHE", "1");
    c
}

#[test]
fn vhdl_2008_force_release_assignments() {
    let f = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vhdl_force_tb.vhd");
    let out = hdlsim()
        .args(["run", "--top", "tb_force"])
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
        stdout.contains("PASS: vhdl-2008 force/release assignments"),
        "{}",
        stdout
    );
    assert!(!stdout.contains("FAIL"), "{}", stdout);
    assert!(
        !stderr.contains("error["),
        "must parse+lower without errors: {}",
        stderr
    );
}
