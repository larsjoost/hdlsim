//! Regression tests for `--trace-assign`: every evaluation of the
//! continuous assignments driving a named signal prints one line with the
//! values the RHS read and the produced result — including re-evaluations
//! whose result equals the current value, because the silence around a
//! sensitivity bug is itself the evidence. Unmatched names, and signals no
//! continuous assignment drives, are hard errors before the run starts.

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
        .args(["run", "--top", "trace_tb"])
        .args(args)
        .arg(fixture("trace_tb.vhd"))
        .output()
        .expect("runs");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Tracing a chain shows every evaluation with its read set: `y` reads `a`,
/// first settles out of X, and the re-evaluations at t=10ns (a changed, the
/// `or x"2"` result did not) print as evaluated-but-unchanged.
#[test]
fn trace_assign_logs_every_evaluation_with_reads() {
    let (_, err) = run(&["--trace-assign", "y"]);
    assert_eq!(err.matches("->y ").count(), 5, "{err}");
    // First evaluation settles out of the initial metavalue...
    assert!(
        err.contains("->y = 0b0011 (was \"xxxx\") reads{a=0b0001}"),
        "{err}"
    );
    // ...and after a <= "0011" the assignment is re-evaluated with the new
    // read but the same result — the unchanged evaluation is the point.
    assert!(
        err.contains("->y = 0b0011 (was 0b0011) reads{a=0b0011}"),
        "{err}"
    );
    assert!(err.contains("t=10ns"), "{err}");
}

/// Tracing the second link of the chain shows it reading the first.
#[test]
fn trace_assign_shows_downstream_reads() {
    let (_, err) = run(&["--trace-assign", "w"]);
    assert_eq!(err.matches("->w ").count(), 5, "{err}");
    assert!(err.contains("reads{y=0b0011}"), "{err}");
    // Only the named link is traced.
    assert!(!err.contains("->y "), "{err}");
}

/// A signal no continuous assignment drives is an arm-time error — the
/// per-evaluation trace cannot see process-driven signals.
#[test]
fn trace_assign_rejects_process_driven_and_unmatched_names() {
    let out = hdlsim()
        .args(["run", "--top", "watch_tb", "--trace-assign", "q"])
        .arg(fixture("watch_tb.vhd"))
        .output()
        .expect("runs");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "must fail the run: {err}");
    assert!(
        err.contains("--trace-assign: no continuous assignment drives q"),
        "{err}"
    );
    assert!(err.contains("--watch or --when"), "{err}");

    let out = hdlsim()
        .args(["run", "--top", "trace_tb", "--trace-assign", "nope"])
        .arg(fixture("trace_tb.vhd"))
        .output()
        .expect("runs");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "must fail the run: {err}");
    assert!(
        err.contains("--trace-assign: no signal matched nope"),
        "{err}"
    );
    assert!(!err.contains("trace-assign t="), "{err}");
}

/// The probes work from `debug` too: trace, examine, and when on one run.
#[test]
fn debug_command_arms_the_probes() {
    let out = hdlsim()
        .args([
            "debug",
            "--top",
            "trace_tb",
            "--trace-assign",
            "y",
            "--examine",
            "y@10ns",
            "--when",
            "a=3",
        ])
        .arg(fixture("trace_tb.vhd"))
        .output()
        .expect("runs");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{err}");
    assert!(err.contains("trace-assign t="), "{err}");
    assert!(err.contains("examine t=10ns y=0b0011"), "{err}");
    assert!(err.contains("when t=10ns"), "{err}");
    assert!(err.contains("a 0b0001->0b0011"), "{err}");
}
