//! Regression test: VHDL `next` skips to the loop step (LRM 1076 §10.12) and
//! `exit` leaves the loop but not the process (§10.11). A regression lowered
//! both to `break`, so the first `next` ended the whole loop.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/{}", name))
}

/// `next` (plain and `next when c;`) must skip only the current iteration;
/// `exit` must end its loop early while the process keeps running.
#[test]
fn next_skips_iteration_and_exit_ends_only_the_loop() {
    let out = hdlsim()
        .args(["run", "--top", "loop_jumps_tb"])
        .arg(fixture("loop_jumps_tb.vhd"))
        .output()
        .expect("runs");
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "run failed:\n{all}");
    // odd: 5 iterations (1,3,5,7,9); even: 5 (0,2,4,6,8);
    // stop: 4 (i=0..3, exit at i=4); tail: statements after the exit ran.
    assert!(all.contains("JUMPS odd=5 even=5 stop=4 tail=1"), "{all}");
    assert!(!all.contains("warning"), "{all}");
}
