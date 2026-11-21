//! Regression tests for function call resolution: overloaded functions
//! (same name, different argument counts) must be matched by arity, and an
//! infinitely recursive call must stop the simulator with a diagnostic
//! instead of overflowing the native stack.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.env("HDLSIM_NO_CACHE", "1");
    c
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn overloaded_functions_match_by_arity() {
    let out = hdlsim()
        .args(["run", "--top", "tb_func_overload"])
        .arg(fixture("vhdl_func_overload_tb.vhd"))
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
    assert!(
        stdout.contains("PASS: overloaded functions matched by arity"),
        "{}",
        stdout
    );
}

#[test]
fn infinite_function_recursion_stops_with_error() {
    let out = hdlsim()
        .args(["run", "--top", "tb_func_recursion"])
        .arg(fixture("vhdl_func_recursion_tb.vhd"))
        .output()
        .expect("runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "expected exit 1, got success\n{}{}",
        stdout,
        stderr
    );
    let all = format!("{}{}", stdout, stderr);
    assert!(
        all.contains("function call depth exceeded"),
        "expected the recursion diagnostic, got:\n{}",
        all
    );
}
