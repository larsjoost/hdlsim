//! Regression tests for constant element access: index and slice bounds on
//! array constants must follow the declared range direction, not the flat
//! value-bit order (element k of an ascending range is value element k, of
//! a descending range value element size-1-k), and single-element reads
//! must const-evaluate instead of falling back to X with a
//! constant-initializer warning.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

/// Ascending/descending/offset-range constants addressed by element reads
/// and slices — every bound follows the declared direction, the runtime
/// slice reads the same elements, and no constant-initializer warning
/// fires.
#[test]
fn const_index_and_slice_follow_range_direction() {
    let f =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vhdl_const_slice_tb.vhd");
    let out = hdlsim()
        .args(["run", "--top", "const_slice_tb"])
        .arg(&f)
        .output()
        .expect("runs");
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "run failed:\n{all}");
    assert!(all.contains("slice_tb: PASS"), "{all}");
    assert!(!all.contains("FAIL"), "{all}");
    assert!(!all.contains("warning"), "{all}");
}
