//! Regression test for `sig'event` (the `clk'event and clk = '1'` idiom) and
//! `wait on`. 'event previously lowered to X, so edge-detect processes never
//! fired and the simulation hung.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

#[test]
fn sig_event_and_wait_on_simulate() {
    let f = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vhdl_event_tb.vhd");
    let out = hdlsim()
        .args(["run", "--top", "tb_event"])
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
    assert!(stdout.contains("PASS: vhdl event"), "{}", stdout);
    assert!(
        !stderr.contains("not supported"),
        "'event must lower without warning: {}",
        stderr
    );
}
