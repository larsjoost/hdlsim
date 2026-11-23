//! End-to-end tests for `hdlsim assist` (prompt -> task router) and
//! `hdlsim explain` (diagnostic-code explanations), driven through the real
//! binary so stdout/stderr and exit codes are observable.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

fn assist(args: &[&str]) -> std::process::Output {
    let mut argv: Vec<&str> = vec!["assist"];
    argv.extend_from_slice(args);
    hdlsim().args(&argv).output().expect("hdlsim assist runs")
}

fn tmp_file(name: &str, content: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("hdlsim_assist_{}_{}", std::process::id(), name));
    std::fs::write(&p, content).unwrap();
    p
}

#[test]
fn explain_known_and_unknown_codes() {
    let out = hdlsim().args(["explain", "E0301"]).output().expect("runs");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("E0301:"), "{}", stdout);
    assert!(stdout.contains("expected"), "{}", stdout);

    // No argument lists every known code.
    let out = hdlsim().args(["explain"]).output().expect("runs");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("W0102"), "{}", stdout);

    // Unknown code: exit 1, error on stderr.
    let out = hdlsim().args(["explain", "Z9999"]).output().expect("runs");
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown code 'Z9999'"));
}

#[test]
fn assist_code_prompt_explains_inline() {
    let out = assist(&["what", "does", "E0301", "mean"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("$ hdlsim explain E0301"), "{}", stdout);
    assert!(stdout.contains("E0301: parser:"), "{}", stdout);
}

#[test]
fn assist_check_prompt_runs_check_and_explains_diagnostics() {
    let bad = tmp_file("bad.v", "module m;\n  wire x\nendmodule\n"); // missing `;`
    let prompt = format!("why does {} fail", bad.display());
    let out = assist(&[&prompt]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stdout.contains("$ hdlsim check"), "{}", stdout);
    assert!(stderr.contains("error[E0301]"), "{}", stderr);
    assert!(stderr.contains("note[E0301]:"), "{}", stderr);
    let _ = std::fs::remove_file(&bad);
}

#[test]
fn assist_suggests_task_commands() {
    let out = assist(&["run", "the", "tests", "--top", "tb_counter"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("$ hdlsim regress --junit results.xml tb_counter"),
        "{}",
        stdout
    );

    let out = assist(&[
        "what",
        "files",
        "does",
        "tb",
        "depend",
        "on",
        "--top",
        "tb_counter",
    ]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("$ hdlsim deps --top tb_counter"),
        "{}",
        stdout
    );

    let out = assist(&["show", "me", "the", "vcd", "trace", "--top", "tb"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("$ hdlsim run --top tb --vcd waves.vcd"),
        "{}",
        stdout
    );
}

#[test]
fn assist_yes_executes_the_suggestion() {
    // A file with trailing whitespace: `assist --yes "lint <file>"` must
    // dispatch `lint --fix` and actually fix the file.
    let f = tmp_file("lintme.v", "module m;\nendmodule\n   \n");
    let prompt = format!("please lint {}", f.display());
    let out = assist(&["--yes", &prompt]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("$ hdlsim lint --fix"), "{}", stdout);
    let after = std::fs::read_to_string(&f).unwrap();
    assert!(
        !after.lines().any(|l| l.ends_with(' ') || l.ends_with('\t')),
        "trailing whitespace remains: {:?}",
        after
    );
    let _ = std::fs::remove_file(&f);
}

#[test]
fn assist_unknown_prompt_lists_tasks() {
    let out = assist(&["frobnicate", "the", "widget"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("no rule matched"), "{}", stdout);
    assert!(stdout.contains("routes prompts to tasks"), "{}", stdout);
}
