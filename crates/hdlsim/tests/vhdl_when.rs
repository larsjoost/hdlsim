//! Regression tests for the `--when` and `--examine` flags.
//! `--when` echoes one line per change of a named signal that matches the
//! spec's value filter and time window (`name[=value][@T|@T0-T1]`);
//! `--examine` prints one settled value at a time point (or the end-of-run
//! value). Unmatched names, unparseable values, and unparseable times are
//! hard errors before the run starts — a typo'd knob must not fail silently.

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

fn run(args: &[&str]) -> (String, String) {
    let out = hdlsim()
        .args(["run", "--top", "watch_tb"])
        .args(args)
        .arg(fixture("watch_tb.vhd"))
        .output()
        .expect("runs");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A value filter prints only transitions into the matched value: `q=1`
/// catches the t=0 init and the t=20 return, not the t=10 excursion into X.
#[test]
fn when_value_filter_prints_only_matching_transitions() {
    let (_, err) = run(&["--when", "q=1"]);
    assert_eq!(err.matches("when t=").count(), 2, "{err}");
    assert!(err.contains("when t=0fs"), "{err}");
    assert!(err.contains("q 0b0000->0b0001"), "{err}");
    assert!(err.contains("q \"xxxx\"->0b0001"), "{err}");
    assert!(!err.contains("0b0001->\"xxxx\""), "{err}");
}

/// An exact-time window (`@T`) prints only the changes at that timestamp.
#[test]
fn when_exact_time_prints_only_that_timestamp() {
    let (_, err) = run(&["--when", "q@10ns"]);
    assert_eq!(err.matches("when t=").count(), 1, "{err}");
    assert!(err.contains("when t=10ns"), "{err}");
    assert!(err.contains("q 0b0001->\"xxxx\""), "{err}");
}

/// A range window (`@T0-T1`) is inclusive on both ends.
#[test]
fn when_time_range_is_inclusive() {
    let (_, err) = run(&["--when", "q@10ns-20ns"]);
    assert_eq!(err.matches("when t=").count(), 2, "{err}");
    assert!(err.contains("when t=10ns"), "{err}");
    assert!(err.contains("when t=20ns"), "{err}");
    assert!(!err.contains("when t=0fs"), "{err}");
}

/// Typos fail the run before simulation starts, with a targeted message.
#[test]
fn when_rejects_unmatched_names_values_and_times() {
    for (spec, frag) in [
        ("nope=1", "--when: no signal matched nope"),
        ("q=zz", "--when: cannot parse value 'zz' for q (width 4)"),
        ("q@abc", "--when: bad time in 'q@abc'"),
    ] {
        let out = hdlsim()
            .args(["run", "--top", "watch_tb", "--when", spec])
            .arg(fixture("watch_tb.vhd"))
            .output()
            .expect("runs");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "'{spec}' must fail the run: {err}");
        assert!(err.contains(frag), "{spec}: {err}");
        assert!(!err.contains("when t="), "{spec}: {err}");
    }
}

/// `--examine sig@T` prints the settled value at exactly T — at the event
/// itself, and (with the value that still holds) between events.
#[test]
fn examine_prints_settled_value_at_and_between_events() {
    let (_, err) = run(&["--examine", "q@10ns", "--examine", "q@5ns"]);
    assert!(err.contains("examine t=10ns q=\"xxxx\""), "{err}");
    assert!(err.contains("examine t=5ns q=0b0001"), "{err}");
    assert_eq!(err.matches("examine t=").count(), 2, "{err}");
}

/// Bare `--examine sig` prints the end-of-run value, labeled as such.
#[test]
fn examine_without_time_prints_end_of_run_value() {
    let (_, err) = run(&["--examine", "q"]);
    assert!(
        err.contains("examine t=30ns q=0b0001 (end-of-run value)"),
        "{err}"
    );
}

/// A time point the bounded run never reached reports the end-of-run value
/// and says the requested time was not reached — never a silent miss. With
/// --max-time 15ns the last applied event is the t=10ns change to "xxxx"
/// (the t=20ns restore never runs).
#[test]
fn examine_past_a_bounded_run_reports_not_reached() {
    let (_, err) = run(&["--max-time", "15ns", "--examine", "q@1us"]);
    assert!(err.contains("examine t=1us q=\"xxxx\""), "{err}");
    assert!(
        err.contains("(end-of-run value; t=1us not reached)"),
        "{err}"
    );
}
