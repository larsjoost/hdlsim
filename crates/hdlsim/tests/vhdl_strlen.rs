//! Regression tests for constant folding of string-building subprograms and
//! the plain `'length` family on constants/generics (LRM 1076 §5.2.1,
//! §7.3.5). Without the fixes the initializers fell back to X with
//! `warning[W0401]: constant initializer is not constant`.

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
fn strlen_constant_folding() {
    let f = fixture("vhdl_strlen_tb.vhd");
    let out = hdlsim()
        .args(["run", "--top", "strlen_tb"])
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
    // Empty strings fold to 0, generic and constant alike.
    assert!(stdout.contains("EMPTY_GEN=0"), "{}", stdout);
    assert!(stdout.contains("EMPTY_CON=0"), "{}", stdout);
    // A constrained string subtype counts characters, not bits (6, not 48).
    assert!(stdout.contains("NAME_LEN=6"), "{}", stdout);
    assert!(stdout.contains("SUB_LEN=6"), "{}", stdout);
    // Constant aggregate dims.
    assert!(stdout.contains("ROW_LEN=5"), "{}", stdout);
    // Record built through field and field-slice assignments.
    assert!(stdout.contains("OPT_ACTIVE=true"), "{}", stdout);
    // A warning report inside a folded body does not abort the fold.
    assert!(stdout.contains("LOG2=4"), "{}", stdout);
    // No X-fallback warning for the folded initializers.
    assert!(!stderr.contains("W0401"), "{}", stderr);
}
