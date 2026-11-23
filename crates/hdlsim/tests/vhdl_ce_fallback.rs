//! Regression tests for const-eval of package-function calls in constant
//! initializers: a named use clause (`use p.f;`) merges only one function,
//! yet its body may call package-internal siblings — the lowerer's
//! package-wide fallback table must resolve them, and loops with `exit` or
//! `next` must keep the value accumulated around the jump. Any failure used
//! to X-poison the whole constant chain with a W0401 warning.

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

/// A named-use constant chain resolves through package-internal sibling
/// calls, and a loop with `exit` keeps its accumulated value — with zero
/// warnings (the unit-test bar: clean run, real values).
#[test]
fn named_use_initializer_const_evals_through_package_internal_calls() {
    let out = hdlsim()
        .args(["run", "--top", "ce_fallback_tb"])
        .arg(fixture("ce_pkg_fallback_pkg.vhd"))
        .arg(fixture("ce_pkg_fallback_tb.vhd"))
        .output()
        .expect("runs");
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "run failed:\n{all}");
    // get_chain_delay = get_stage_delay(true) + 1 = get_leaf_delay + 1 = 6.
    assert!(all.contains("C_CHAIN=6"), "{all}");
    // get_first_stop_delay(10): +2,+2,+2 then +1 and exit at i=3 -> 7.
    assert!(all.contains("C_STOP=7"), "{all}");
    // get_odd_count_delay(10): `next` skips even i -> odds 1,3,5,7,9 -> 5.
    assert!(all.contains("C_ODD=5"), "{all}");
    // get_even_count_delay(10): `next when` skips odd i -> 5.
    assert!(all.contains("C_EVEN=5"), "{all}");
    assert!(!all.contains("warning"), "{all}");
    assert!(!all.contains("W0401"), "{all}");
}
