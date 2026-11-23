//! Regression tests for `HDLSIM_WATCH`: the kernel-side signal-change
//! watch. The watch prints one line per change of the watched signals
//! (with sim time, delta number, old->new value, and the driving
//! process), an x-only mode that shows only transitions into
//! metavarues, per-timestamp snapshots, and a line cap so a runaway
//! design cannot flood the log.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.env("HDLSIM_NO_CACHE", "1");
    c
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/{}", name))
}

/// Default mode: every change of a watched signal prints with time,
/// delta, old->new values, and the driver origin.
#[test]
fn watch_prints_each_change_with_time_value_and_driver() {
    let out = hdlsim()
        .args(["run", "--top", "watch_tb"])
        .arg(fixture("watch_tb.vhd"))
        .env("HDLSIM_WATCH", "q")
        .output()
        .expect("runs");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "run failed:\n{stderr}");
    assert!(stderr.contains("watch: 1 signal(s)"), "{stderr}");
    // The three changes: 0000->0001, 0001->"xxxx", "xxxx"->0001.
    assert!(stderr.contains("0b0000->0b0001"), "{stderr}");
    assert!(stderr.contains("0b0001->\"xxxx\""), "{stderr}");
    assert!(stderr.contains("\"xxxx\"->0b0001"), "{stderr}");
    // Driver attribution: the single process of the testbench.
    assert!(stderr.contains("driver="), "{stderr}");
    assert!(stderr.contains("watch_tb.vhd:"), "{stderr}");
    // Timestamps land in the lines.
    assert!(stderr.contains("watch t=10ns"), "{stderr}");
}

/// A name that matches nothing is reported, not silently ignored.
#[test]
fn watch_reports_unmatched_names() {
    let out = hdlsim()
        .args(["run", "--top", "watch_tb"])
        .arg(fixture("watch_tb.vhd"))
        .env("HDLSIM_WATCH", "no_such_signal")
        .output()
        .expect("runs");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(
        stderr.contains("watch: no signal matched no_such_signal"),
        "{stderr}"
    );
}

/// x-only mode: transitions into metavarues print, clean changes do not.
#[test]
fn watch_x_only_shows_only_metavalue_transitions() {
    let out = hdlsim()
        .args(["run", "--top", "watch_tb"])
        .arg(fixture("watch_tb.vhd"))
        .env("HDLSIM_WATCH", "q")
        .env("HDLSIM_WATCH_X", "1")
        .output()
        .expect("runs");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(stderr.contains("0b0001->\"xxxx\""), "{stderr}");
    assert!(!stderr.contains("0b0000->0b0001"), "{stderr}");
    assert!(!stderr.contains("\"xxxx\"->0b0001"), "{stderr}");
}

/// Snapshot mode: settled values of all watched signals per timestamp,
/// including signals that did not change in that timestamp.
#[test]
fn watch_snapshots_print_settled_values_per_timestamp() {
    let out = hdlsim()
        .args(["run", "--top", "watch_tb"])
        .arg(fixture("watch_tb.vhd"))
        .env("HDLSIM_WATCH", "q,a")
        .env("HDLSIM_WATCH_DELTAS", "1")
        .output()
        .expect("runs");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(stderr.contains("watch-snap"), "{stderr}");
    // At t=10ns the settled state is q="xxxx", a unchanged (0b0001).
    assert!(stderr.contains("watch-snap t=10ns"), "{stderr}");
    let snap = stderr
        .lines()
        .find(|l| l.starts_with("watch-snap t=10ns"))
        .expect("t=10ns snapshot line");
    assert!(snap.contains("q=\"xxxx\""), "{snap}");
    assert!(snap.contains("a=0b0001"), "{snap}");
}

/// The line cap stops a flood: once the budget is exhausted, a one-time
/// notice prints and further lines are suppressed.
#[test]
fn watch_line_cap_suppresses_after_the_budget() {
    let out = hdlsim()
        .args(["run", "--top", "watch_tb"])
        .arg(fixture("watch_tb.vhd"))
        .env("HDLSIM_WATCH", "q,a")
        .env("HDLSIM_WATCH_MAX", "2")
        .output()
        .expect("runs");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(stderr.contains("watch: line cap (2) reached"), "{stderr}");
    let watch_lines = stderr.lines().filter(|l| l.starts_with("watch ")).count();
    assert_eq!(watch_lines, 2, "cap must hold, got:\n{stderr}");
}
