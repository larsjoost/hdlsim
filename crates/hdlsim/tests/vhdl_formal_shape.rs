//! Regression tests for shape-at-binding of unconstrained 1-D vector
//! formals: an unconstrained `bit_vector` formal takes the actual's index
//! range (LRM 1076 §5.2.1), so element and slice writes and reads through
//! the formal must land at the actual's layout — ascending and descending
//! actuals alike — at runtime and inside const-eval'd constant initializers,
//! including chained calls whose actual is itself a shape-bound formal and
//! reads whose actual is a constant.
//!
//! Boundary (slice 3a): bit-vector-like (element-width 1) formals only.
//! String/8-bit-element formals, multi-dimensional arrays, `'length`/`'range`
//! attributes on formals, and literal actuals (no bind — the callee falls
//! back to its recorded convention) are later slices.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

/// Element and slice writes through an unconstrained vector formal reach
/// the caller's variable at the actual's index layout (ascending and
/// descending actuals), reads through the formal see the actual's value,
/// chained formals keep the outer frame's binding, and the same operations
/// const-eval inside constant initializers.
#[test]
fn unconstrained_vector_formal_takes_the_actuals_index_range() {
    let f =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vhdl_formal_shape_tb.vhd");
    let out = hdlsim()
        .args(["run", "--top", "formal_shape_tb"])
        .arg(&f)
        .output()
        .expect("runs");
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "run failed:\n{all}");
    assert!(all.contains("formal_shape_tb: PASS"), "{all}");
    assert!(!all.contains("FAIL"), "{all}");
    assert!(!all.contains("wrong"), "{all}");
}
