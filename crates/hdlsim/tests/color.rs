//! Colorized diagnostics: `--color always|never|auto`, `--no-color`, and the
//! standard `NO_COLOR` variable. Colors are on by default — even for piped
//! output (agent-consumed transcripts) — and the default honors `NO_COLOR`.

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
fn piped_default_is_colored() {
    // The default is always-on: piped output still carries the severity
    // color (agent-consumed transcripts). `--no-color` / NO_COLOR turn it
    // off, `--color auto` restores tty detection.
    let f = write_tmp("default", BAD_SRC);
    let (_o, err, _c) = run_color(&[], &f, &[]);
    assert!(err.contains("\x1b[1;31m"), "{}", err);
    let _ = std::fs::remove_file(&f);

    // NO_COLOR disables the default mode.
    let f = write_tmp("default_nc", BAD_SRC);
    let (_o, err, _c) = run_color(&[], &f, &[("NO_COLOR", "1")]);
    assert!(!err.contains("\x1b["), "{}", err);
    let _ = std::fs::remove_file(&f);

    // An explicit --color always overrides NO_COLOR.
    let f = write_tmp("always_nc", BAD_SRC);
    let (_o, err, _c) = run_color(&["--color", "always"], &f, &[("NO_COLOR", "1")]);
    assert!(err.contains("\x1b[1;31m"), "{}", err);
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
fn no_color_env_disables_the_default() {
    // NO_COLOR applies to the default (always-on) mode; an explicit
    // `--color never` is plain everywhere regardless.
    let f = write_tmp("nocolor", BAD_SRC);
    let (_o, err, _c) = run_color(&[], &f, &[("NO_COLOR", "1")]);
    assert!(!err.contains("\x1b["), "{}", err);
    let (_o, err, _c) = run_color(&["--color", "never"], &f, &[("NO_COLOR", "1")]);
    assert!(!err.contains("\x1b["), "{}", err);
    let _ = std::fs::remove_file(&f);
}
