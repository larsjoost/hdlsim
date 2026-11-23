//! Regression test: package function calls in constant initializers are
//! const-evaluated (the function body is interpreted with the arguments
//! bound to slots), e.g. `constant W : natural := bits_f(8);` driving a
//! vector width. Covers if/return and for-loop/2** bodies.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pkg_constfn")
}

#[test]
fn package_function_calls_in_constant_initializers() {
    let dir = fixtures();
    let out = hdlsim()
        .args(["run", "--top", "tb_constfn", "-L"])
        .arg(format!("work={}", dir.display()))
        .arg(dir.join("top.vhd"))
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
        stdout.contains("PASS: vhdl const function calls"),
        "{}",
        stdout
    );
    assert!(!stderr.contains("unknown identifier"), "{}", stderr);
}

/// Regression test for the issue #106 follow-up warnings: a function-local
/// constant with a parameter-dependent initializer
/// (`constant rmsb : integer := size - 1;`, as in Vivado's std_logic_arith)
/// must check with zero warnings AND evaluate per call at runtime.
#[test]
fn function_local_param_dependent_constant_checks_clean_and_evaluates_per_call() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pkg_fnlocalconst");
    let lib = format!("work={}", dir.display());

    let out = hdlsim()
        .args(["check", "-L", &lib])
        .arg(dir.join("top.vhd"))
        .output()
        .expect("runs");
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{}{}", all, "");
    assert!(!all.contains("warning["), "{}", all);
    assert!(!all.contains("error["), "{}", all);

    let out = hdlsim()
        .args(["run", "--top", "tb_fnlocalconst", "-L", &lib])
        .arg(dir.join("top.vhd"))
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
        stdout.contains("PASS: fn-local const per call"),
        "{}",
        stdout
    );
}
