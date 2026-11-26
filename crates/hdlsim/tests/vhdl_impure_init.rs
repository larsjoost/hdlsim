//! Regression tests for the constant-initializer warning (W0401) and
//! impure-function initializers. An impure function's value is legal VHDL —
//! evaluated once, at elaboration — but the const-eval cannot execute
//! impure code, so the X fallback there is by design and the warning stays
//! silent. A pure-function initializer that fails to fold keeps warning:
//! it signals a genuine const-eval gap worth reporting.

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

/// An impure-function initializer (parameterless and argument-position
/// forms) folds to X without the constant-initializer warning, and the run
/// stays clean.
#[test]
fn impure_function_initializer_stays_silent() {
    let out = hdlsim()
        .args(["run", "--top", "impure_init_tb"])
        .arg(fixture("impure_init_tb.vhd"))
        .output()
        .expect("runs");
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "run failed:\n{all}");
    // The documented X fallback: the constant is bound to unknown.
    assert!(all.contains("C_SEED=X"), "{all}");
    assert!(!all.contains("warning"), "{all}");
    assert!(!all.contains("W0401"), "{all}");
}

/// A pure-function initializer that cannot fold keeps the warning —
/// exactly one, whatever the call form.
#[test]
fn pure_function_initializer_that_fails_keeps_warning() {
    let out = hdlsim()
        .args(["run", "--top", "pure_init_fails_tb"])
        .arg(fixture("pure_init_fails_tb.vhd"))
        .output()
        .expect("runs");
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "run failed:\n{all}");
    assert_eq!(
        all.matches("warning[W0401]").count(),
        1,
        "expected exactly one W0401:\n{all}"
    );
}
