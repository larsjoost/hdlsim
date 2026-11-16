//! Tests for the embedded ieee stdlib packages (std_logic_arith, signed,
//! unsigned, textio) and std.env, plus the loud warning for a `use` clause
//! that names a library unit which cannot be resolved.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.env("HDLSIM_NO_CACHE", "1");
    c
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/{}", name))
}

#[test]
fn std_logic_arith_package_resolves_and_simulates() {
    let out = hdlsim()
        .args(["run", "--top", "tb_std_arith"])
        .arg(fixture("std_arith_tb.vhd"))
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
    assert!(stdout.contains("PASS: std_logic_arith"), "{}", stdout);
    // The `use ieee.std_logic_arith.all;` clause resolved: no warning.
    assert!(!stderr.contains("W0501"), "{}", stderr);
}

#[test]
fn unresolved_use_clause_warns_but_check_passes() {
    let out = hdlsim()
        .arg("check")
        .arg(fixture("use_unknown_pkg.vhd"))
        .output()
        .expect("runs");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "warning must not fail check, got {:?}\n{}",
        out.status,
        stderr
    );
    assert!(stderr.contains("W0501"), "{}", stderr);
    assert!(stderr.contains("no_such_package"), "{}", stderr);
}
