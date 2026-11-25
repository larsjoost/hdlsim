//! Regression tests for the function-formal slot-id space: function formals
//! share flat signal ids with module signals, so lowering a function body
//! must (a) not erase record mappings of record-typed signals/ports, and
//! (b) reset its own slot mappings afterwards — a later function's scalar
//! formal must not inherit an earlier function's record type.

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

#[test]
fn record_field_selects_survive_function_lowering() {
    let f = fixture("vhdl_fn_slot_rec_tb.vhd");
    let out = hdlsim()
        .args(["run", "--top", "fn_slot_tb"])
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
    // No E0401: the record signal's mapping must survive the package-body
    // functions being lowered (their formals share its slot ids).
    assert!(!stderr.contains("E0401"), "{}", stderr);
    assert!(stdout.contains("TAG=5"), "{}", stdout);
    // mix(1, 2, pick(cells, 1)) = 1 + 2*5 = 11, minus 2 = 9.
    assert!(stdout.contains("PICK=9"), "{}", stdout);
}
