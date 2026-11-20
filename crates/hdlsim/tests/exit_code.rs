//! CLI exit codes: `hdlsim run` exits 0 when all tests pass and non-zero when
//! a VHDL/Verilog unit test fails (`assert`/`report` severity error|failure,
//! Verilog `$error`/`$fatal`) — so CI and scripts catch failing tests.

use std::process::Command;

fn hdlsim() -> Command {
    Command::new(env!("CARGO_BIN_EXE_hdlsim"))
}

fn run(top: &str, file: &str) -> std::process::Output {
    hdlsim()
        .args(["run", "--top", top, file])
        .output()
        .expect("hdlsim binary runs")
}

#[test]
fn passing_tests_exit_zero() {
    for (top, f) in [
        ("tb_pass", "tests/fixtures/tb_pass.vhd"),
        ("tb_pass_v", "tests/fixtures/tb_pass.v"),
    ] {
        let out = run(top, f);
        assert!(
            out.status.success(),
            "{}: expected exit 0, got {:?}\n{}",
            f,
            out.status,
            String::from_utf8_lossy(&out.stdout)
        );
    }
}

#[test]
fn failing_tests_exit_nonzero() {
    let cases = [
        ("tb_fail_err", "tests/fixtures/tb_fail_err.vhd", "FAIL"),
        (
            "tb_fail_fatal",
            "tests/fixtures/tb_fail_fatal.vhd",
            "assertion failed",
        ),
        ("tb_fail_error", "tests/fixtures/tb_fail_error.v", "FAIL"),
        ("tb_fail_fatal_v", "tests/fixtures/tb_fail_fatal.v", "FAIL"),
    ];
    for (top, f, needle) in cases {
        let out = run(top, f);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            !out.status.success(),
            "{}: expected non-zero exit, got {:?}\n{}",
            f,
            out.status,
            stdout
        );
        assert!(stdout.contains(needle), "{}: {}", f, stdout);
    }
}

#[test]
fn finish_status_becomes_exit_code() {
    // VHDL-2008 std.env.finish(status): the argument is the exit code.
    let out = run("tb_finish_status", "tests/fixtures/tb_finish_status.vhd");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("PASS"),
        "fixture should pass before finishing:\n{}",
        stdout
    );
    assert_eq!(
        out.status.code(),
        Some(3),
        "std.env.finish(3) must exit 3\n{}",
        stdout
    );
}

#[test]
fn verilog_finish_arg_is_not_exit_code() {
    // Verilog $finish(1): the argument is a diagnostic level, not a status —
    // the run still exits 0.
    let out = run("tb_finish_v_arg", "tests/fixtures/tb_finish_v_arg.v");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("PASS"),
        "fixture should pass before finishing:\n{}",
        stdout
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "$finish(1) must not turn into exit code 1\n{}",
        stdout
    );
}

#[test]
fn failures_print_report_hint() {
    // Usage error (exit 2): hint with the bundle command and the issue link.
    let out = hdlsim().args(["frobnicate"]).output().expect("runs");
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("hdlsim bundle"), "{}", err);
    assert!(err.contains("issues/new"), "{}", err);

    // Failing simulation (exit 1): hint as well.
    let out = run("tb_fail_err", "tests/fixtures/tb_fail_err.vhd");
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("issues/new"), "{}", err);

    // Passing run: no hint.
    let out = run("tb_pass", "tests/fixtures/tb_pass.vhd");
    assert!(out.status.success());
    assert!(!String::from_utf8_lossy(&out.stderr).contains("issues/new"));
}
