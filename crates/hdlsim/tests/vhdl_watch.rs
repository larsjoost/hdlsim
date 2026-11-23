//! Regression tests for `--watch`: the kernel-side signal-change watch.
//! The watch prints one line per change of the watched signals (with sim
//! time, delta number, old->new value, and the driving process), an
//! x-only mode that shows only transitions into metavarues,
//! per-timestamp snapshots, and a line cap so a runaway design cannot
//! flood the log. Options are CLI flags, so typos fail loudly: an
//! unmatched signal name is a hard error before the run starts.

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

/// Default mode: every change of a watched signal prints with time,
/// delta, old->new values, and the driver origin.
#[test]
fn watch_prints_each_change_with_time_value_and_driver() {
    let out = hdlsim()
        .args(["run", "--top", "watch_tb", "--watch", "q"])
        .arg(fixture("watch_tb.vhd"))
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

/// A name that matches nothing is a hard error before the run starts —
/// the CLI flag must not fail silently the way an environment variable
/// would.
#[test]
fn watch_rejects_unmatched_names_with_a_clear_error() {
    let out = hdlsim()
        .args(["run", "--top", "watch_tb", "--watch", "q,no_such_signal"])
        .arg(fixture("watch_tb.vhd"))
        .output()
        .expect("runs");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "typo in --watch must fail the run, got success:\n{stderr}"
    );
    assert!(
        stderr.contains("--watch: no signal matched no_such_signal"),
        "{stderr}"
    );
    // The run must not have started: no watch lines were printed.
    assert!(!stderr.contains("watch t="), "{stderr}");
}

/// x-only mode: transitions into metavarues print, clean changes do not.
#[test]
fn watch_x_only_shows_only_metavalue_transitions() {
    let out = hdlsim()
        .args(["run", "--top", "watch_tb", "--watch", "q", "--watch-x"])
        .arg(fixture("watch_tb.vhd"))
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
        .args([
            "run",
            "--top",
            "watch_tb",
            "--watch",
            "q,a",
            "--watch-deltas",
        ])
        .arg(fixture("watch_tb.vhd"))
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
    let mut child = hdlsim()
        .args([
            "run",
            "--top",
            "watch_tb",
            "--watch",
            "q,a",
            "--watch-max",
            "2",
        ])
        .arg(fixture("watch_tb.vhd"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawns");

    // Bounded wait: a regression that wedges the kernel must fail this
    // test loudly instead of hanging the suite.
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        match child.try_wait().expect("poll child") {
            Some(_) => break,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                panic!("watch run did not terminate");
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
    let mut stderr = String::new();
    if let Some(mut p) = child.stderr.take() {
        let _ = p.read_to_string(&mut stderr);
    }
    assert!(stderr.contains("watch: line cap (2) reached"), "{stderr}");
    let watch_lines = stderr.lines().filter(|l| l.starts_with("watch ")).count();
    assert_eq!(watch_lines, 2, "cap must hold, got:\n{stderr}");
}
