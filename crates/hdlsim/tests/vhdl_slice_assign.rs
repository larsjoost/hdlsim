//! Regression test: a signal assignment whose LHS is a slice with reversed
//! *dynamic* bounds (`q(0 downto j)` with j > 0). The write path clamped
//! nothing: `msb - lsb` underflowed to `usize::MAX` and the per-bit write
//! loop spun forever inside a single `assign_to` call — no pump-level
//! interrupt check, heartbeat, or `--max-wall` could observe it. The fix
//! clamps both bounds exactly like the read path (`Value::slice`).

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/{}", name))
}

#[test]
fn reversed_dynamic_slice_assign_terminates_and_writes_the_clamped_bit() {
    let mut child = hdlsim()
        .args(["run", "--top", "slice_rev_tb"])
        .arg(fixture("slice_rev_tb.vhd"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawns");

    // Bounded wait: the pre-fix behavior is an infinite no-op loop, which
    // must fail this test loudly instead of hanging the suite.
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        match child.try_wait().expect("poll child") {
            Some(_status) => break,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                panic!(
                    "reversed-slice assign did not terminate — the write loop spun past its clamp"
                );
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut p) = child.stdout.take() {
        let _ = p.read_to_string(&mut stdout);
    }
    if let Some(mut p) = child.stderr.take() {
        let _ = p.read_to_string(&mut stderr);
    }

    assert!(
        stdout.contains("slice_rev PASS"),
        "clamped bit must be written (a(0)='1' -> q(0)='1'), got:\n{stdout}\n{stderr}"
    );
    assert!(!stdout.contains("slice_rev FAIL"), "{stdout}\n{stderr}");
}
