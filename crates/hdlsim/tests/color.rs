//! Colorized diagnostics: `--color always|never|auto`, `--no-color`, and the
//! standard `NO_COLOR` variable. Tests run the binary with piped output, so
//! auto mode (default) must produce no ANSI escapes.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

const BAD_SRC: &str =
    "module tb;\n  wire x\n  initial begin $display(\"PASS\"); $finish; end\nendmodule\n";
const FAIL_SRC: &str =
    "module tb;\n  initial begin assert (1 == 2); $display(\"done\"); $finish; end\nendmodule\n";

fn write_tmp(name: &str, src: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let p = std::env::temp_dir().join(format!(
        "hdlsim_color_{}_{}_{}.sv",
        std::process::id(),
        n,
        name
    ));
    std::fs::write(&p, src).unwrap();
    p
}

fn run_color(args: &[&str], file: &PathBuf, envs: &[(&str, &str)]) -> (String, String, i32) {
    let mut c = hdlsim();
    let mut full = vec!["run", "--top", "tb"];
    full.extend_from_slice(args);
    c.args(&full).arg(file);
    for (k, v) in envs {
        c.env(k, v);
    }
    let out = c.output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.code().unwrap_or(-1),
    )
}

#[test]
fn color_always_emits_ansi_for_errors() {
    let f = write_tmp("always", BAD_SRC);
    let (_out, err, code) = run_color(&["--color", "always"], &f, &[]);
    assert_eq!(code, 1);
    assert!(err.contains("\x1b[1;31merror[E0301]:\x1b[0m"), "{}", err);
    assert!(err.contains("\x1b[1;31m^^"), "{}", err); // caret run in red
    assert!(err.contains("\x1b[0m"), "{}", err);
    assert!(
        err.contains("error: could not compile due to 1 previous error"),
        "{}",
        err
    );
    let _ = std::fs::remove_file(&f);
}

#[test]
fn color_never_and_no_color_emit_plain_text() {
    let f = write_tmp("never", BAD_SRC);
    let (_o, err, _c) = run_color(&["--color", "never"], &f, &[]);
    assert!(!err.contains("\x1b["), "{}", err);
    assert!(err.contains("error[E0301]:"), "{}", err);

    let (_o, err, _c) = run_color(&["--no-color"], &f, &[]);
    assert!(!err.contains("\x1b["), "{}", err);
    let _ = std::fs::remove_file(&f);
}

#[test]
fn color_flag_after_command_also_works() {
    let f = write_tmp("after", BAD_SRC);
    // Flag after the subcommand args must still be picked up (global flag).
    let mut c = hdlsim();
    let out = c
        .args(["run", "--top", "tb", "--color=always"])
        .arg(&f)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(err.contains("\x1b[1;31m"), "{}", err);
    let _ = std::fs::remove_file(&f);
}

#[test]
fn hdls_sim_color_flag_forces_and_disables() {
    let f = write_tmp("env", BAD_SRC);
    let (_o, err, _c) = run_color(&["--color", "always"], &f, &[]);
    assert!(err.contains("\x1b[1;31merror"), "{}", err);
    let (_o, err, _c) = run_color(&["--color", "never"], &f, &[]);
    assert!(!err.contains("\x1b["), "{}", err);
    let _ = std::fs::remove_file(&f);
}

#[test]
fn piped_default_is_plain() {
    // Test harness pipes output (not a tty), so auto mode must not colorize.
    let f = write_tmp("auto", BAD_SRC);
    let (_o, err, _c) = run_color(&[], &f, &[]);
    assert!(!err.contains("\x1b["), "{}", err);
    assert!(err.contains("error[E0301]:"), "{}", err);
    let _ = std::fs::remove_file(&f);
}

#[test]
fn sim_failures_are_red_when_color_forced() {
    let f = write_tmp("simfail", FAIL_SRC);
    let (out, _err, code) = run_color(&["--color", "always"], &f, &[]);
    assert_eq!(code, 1);
    assert!(out.contains("\x1b[1;31m"), "{}", out);
    assert!(out.contains("assertion failed"), "{}", out);
    assert!(out.contains("\x1b[1;31mhdlsim: FAILED"), "{}", out);
    // Plain $display output is not colorized.
    assert!(
        out.contains("\ndone\n") || out.contains("done\n"),
        "{}",
        out
    );
    assert!(!out.contains("\x1b[1;31mdone"), "{}", out);
    let _ = std::fs::remove_file(&f);
}

#[test]
fn no_color_env_disables_even_when_forced_by_tty() {
    // NO_COLOR only applies in auto mode; with an explicit flag the flag wins,
    // but in the piped test harness auto+NO_COLOR must be plain.
    let f = write_tmp("nocolor", BAD_SRC);
    let (_o, err, _c) = run_color(&[], &f, &[("NO_COLOR", "1")]);
    assert!(!err.contains("\x1b["), "{}", err);
    let _ = std::fs::remove_file(&f);
}
