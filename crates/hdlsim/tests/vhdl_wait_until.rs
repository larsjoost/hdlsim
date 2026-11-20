//! Regression tests for `wait until` semantics (LRM 8.1):
//! - `wait until cond for T` wakes on the condition's edge before the timeout,
//! - the timeout expression is evaluated once when the wait executes,
//! - the condition is re-checked after each wake (a falling edge must not wake
//!   `wait until s = '1'`),
//! - a pre-true condition without an event must not wake the process,
//! - `time` values keep full 64-bit femtosecond magnitude (30 ms ≈ 3e10 fs).

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.env("HDLSIM_NO_CACHE", "1");
    c
}

fn run(top: &str, file: &str, extra: &[&str]) -> (std::process::Output, String, String) {
    let f = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(file);
    let out = hdlsim()
        .args(["run", "--top", top])
        .args(extra)
        .arg(&f)
        .output()
        .expect("hdlsim binary runs");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    (out, stdout, stderr)
}

#[test]
fn wait_until_for_wakes_on_edge_before_timeout() {
    let (out, stdout, stderr) = run(
        "tb_wu_edge",
        "tests/fixtures/vhdl_wait_until_edge_tb.vhd",
        &[],
    );
    assert!(
        out.status.success() && stdout.contains("PASS"),
        "edge wake must beat the timeout:\n{}{}",
        stdout,
        stderr
    );
}

#[test]
fn wait_until_for_timeout_fires_when_no_event() {
    let (out, stdout, stderr) = run(
        "tb_wu_timeout",
        "tests/fixtures/vhdl_wait_until_timeout_tb.vhd",
        &[],
    );
    assert!(
        out.status.success() && stdout.contains("PASS"),
        "expression timeout (C_TIMEOUT - now) must fire:\n{}{}",
        stdout,
        stderr
    );
}

#[test]
fn wait_until_rechecks_condition_on_edge() {
    let (out, stdout, stderr) = run(
        "tb_wu_recheck",
        "tests/fixtures/vhdl_wait_until_recheck_tb.vhd",
        &[],
    );
    assert!(
        out.status.success() && stdout.contains("PASS"),
        "falling edges must not wake `wait until s = '1'`:\n{}{}",
        stdout,
        stderr
    );
}

#[test]
fn pre_true_wait_until_does_not_wake() {
    let (out, stdout, stderr) = run(
        "tb_wu_notrig",
        "tests/fixtures/vhdl_wait_until_notrig_tb.vhd",
        &["--max-time", "80ns"],
    );
    assert!(
        stdout.contains("PASS") && !stdout.contains("FAIL"),
        "pre-true condition must stay parked:\n{}{}",
        stdout,
        stderr
    );
}

#[test]
fn time_constants_keep_full_magnitude() {
    let (out, stdout, stderr) = run(
        "tb_time_width",
        "tests/fixtures/vhdl_time_width_tb.vhd",
        &[],
    );
    assert!(
        out.status.success() && stdout.contains("PASS"),
        "30 ms must not truncate to 32 bits:\n{}{}",
        stdout,
        stderr
    );
}
