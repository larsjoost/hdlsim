//! Regression test: a record field whose type is a constrained array
//! (`field_description : fields_t` where `fields_t is array (field_id) of
//! field_t`) must take the array's full flat width, not one element. The
//! width used to collapse to a single element, so field-of-array selects
//! folded to X past index 0 and index 0 silently read the last element —
//! caught root-causing W0401s in a third-party-derived design.

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
fn constrained_array_record_field_selects_fold() {
    let f = fixture("vhdl_rec_arr_field_tb.vhd");
    let out = hdlsim()
        .args(["run", "--top", "rec_arr_field_tb"])
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
    // Enum-literal and integer indexes both select the FIRST element's
    // bit_size (8), and the enum index past 0 folds (16) instead of X.
    assert!(stdout.contains("FIRST=8"), "{}", stdout);
    assert!(stdout.contains("SECOND=16"), "{}", stdout);
    assert!(stdout.contains("INT=8"), "{}", stdout);
    // No fold failures from the field-select chain.
    assert!(!stderr.contains("W0401"), "{}", stderr);
}
