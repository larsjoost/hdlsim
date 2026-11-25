//! Regression test: a constant of a constrained array-of-record type built
//! from an enum-choice aggregate with OTHERS must cover every element, and a
//! record field of that array type must carry the full flat width. When the
//! array type mark collapsed to its element width, the aggregate shrank to
//! one element and record-field selects folded to silently wrong values
//! (status ok, value 0) — no warning, just a wrong number.

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
fn enum_choice_array_aggregate_and_record_field_fold() {
    let f = fixture("vhdl_arr_choice_tb.vhd");
    let out = hdlsim()
        .args(["run", "--top", "arr_choice_tb"])
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
    // Chosen elements fold to their own values (not the OTHERS default),
    // and the OTHERS element folds too.
    assert!(stdout.contains("B=8"), "{}", stdout);
    assert!(stdout.contains("D=4"), "{}", stdout);
    assert!(stdout.contains("A=1"), "{}", stdout);
    assert!(!stderr.contains("W0401"), "{}", stderr);
}
