//! Regression test for sequential signal assignments with `after` delays
//! (`s <= '1' after 10 ns;`). The delay used to be silently dropped (the delay
//! expression itself was lowered as the assigned value, so the signal never
//! got the intended value); now each waveform element schedules a transaction
//! at now + its own delay without suspending the process.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

#[test]
fn seq_assign_after_delay_simulates() {
    let f = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vhdl_after_tb.vhd");
    let out = hdlsim()
        .args(["run", "--top", "tb_after"])
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
    assert!(stdout.contains("PASS: vhdl after-delay"), "{}", stdout);
    assert!(
        !stderr.contains("not supported"),
        "`after` must lower without warning: {}",
        stderr
    );
}
