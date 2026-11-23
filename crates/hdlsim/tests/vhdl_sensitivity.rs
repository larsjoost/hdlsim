//! Regression test: sensitivity-list entries that are indexed names
//! (`data(1)`, `data(i)` in a generate) must wake the process. The parser
//! wraps postfix indexing in a Call node; dropping those entries left the
//! process with an empty sensitivity list — an infinite zero-delay loop at
//! t=0 (`zero-delay process loop detected`).

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

fn run_clean(top: &str, file: &PathBuf, marker: &str) {
    let out = hdlsim()
        .args(["run", "--top", top, "--max-time", "10ms"])
        .arg(file)
        .output()
        .expect("runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "{top} must simulate cleanly, run said:\n{stdout}\n{stderr}"
    );
    assert!(!stdout.contains("zero-delay"), "{stdout}\n{stderr}");
    assert!(!stdout.contains("error"), "{stdout}\n{stderr}");
    assert!(!stderr.contains("error"), "{stdout}\n{stderr}");
    assert!(!stderr.contains("warning"), "{stdout}\n{stderr}");
    assert!(stdout.contains(marker), "{stdout}\n{stderr}");
}

/// A `PROCESS (data(i))` entry in a generate loop resolves to the base
/// signal and the process wakes when any element changes.
#[test]
fn indexed_sensitivity_entry_in_generate_wakes_the_process() {
    run_clean("sens_idx_tb", &fixture("sens_idx_tb.vhd"), "SENSIDX done");
}
