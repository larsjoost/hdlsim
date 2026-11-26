//! Regression tests for the constant-initializer warning (W0401) and
//! impure-function initializers. The const-eval executes procedure bodies
//! (with inout/out copy-back), so an effect-free impure function folds to
//! its real value; what it cannot know (a signal read) falls back to X.
//! Both impure outcomes stay silent — the value is not a fold gap but
//! genuinely elaboration-time. A pure-function initializer that fails to
//! fold keeps warning: it signals a genuine const-eval gap worth reporting.

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

/// An impure-function initializer folds when its body is effect-free (the
/// procedure call runs with copy-back) and falls back to X when it reads a
/// signal — both without the constant-initializer warning, and the run
/// stays clean.
#[test]
fn impure_initializer_folds_or_falls_back_silently() {
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
    // Effect-free body: the constant holds the procedure's result (0 + 40 + 2).
    assert!(all.contains("C_SEED=42"), "{all}");
    // Signal read: the documented X fallback, silent.
    assert!(all.contains("C_LIVE=x"), "{all}");
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
