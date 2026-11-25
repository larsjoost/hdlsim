//! Regression tests for dimension-suffixed array attributes — `A'length(1)`,
//! `A'high(2)`, … (LRM 1076 §5.2.1) — folding in constant expressions when
//! the dimension's bounds are statically known: an aggregate-typed generic
//! default, a package constant with an inline decl constraint (dims cross
//! files with the package), and an array-typed signal. Without the fix the
//! initializer fell back to X with `warning[W0401]: constant initializer is
//! not constant`.

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
fn length_with_dimension_folds() {
    let f = fixture("vhdl_len_dim_tb.vhd");
    let out = hdlsim()
        .args(["run", "--top", "len_dim_tb"])
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
    // Generic with a 2-D aggregate default: 2 rows × 3 columns.
    assert!(stdout.contains("ROWS=2"), "{}", stdout);
    assert!(stdout.contains("COLS=3"), "{}", stdout);
    assert!(stdout.contains("MULTI=1"), "{}", stdout);
    // Package constant with an inline constraint (cross-file dims).
    assert!(stdout.contains("ORIG_ROWS=2"), "{}", stdout);
    assert!(stdout.contains("ORIG_COLS=3"), "{}", stdout);
    // Array-typed signal.
    assert!(stdout.contains("SIG_COLS=2"), "{}", stdout);
    assert!(stdout.contains("VEC_LEN=2"), "{}", stdout);
    // No X-fallback warning for the folded initializers.
    assert!(!stderr.contains("W0401"), "{}", stderr);
}
