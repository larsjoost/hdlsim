//! AI-debugging ergonomics end-to-end: machine-readable diagnostics
//! (`--error-format json`, one JSON object per line on stderr) and source
//! locations on runtime failures (transcript lines get a `file:line:` prefix
//! and the FAILED summary names the first failure's location).

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

fn tmp(name: &str, content: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("hdlsim_ai_debug_{}_{}", std::process::id(), name));
    std::fs::write(&p, content).unwrap();
    p
}

#[test]
fn error_format_json_emits_parseable_json_lines() {
    let f = tmp("bad_syntax.v", "module m;\n  wire x\nendmodule\n");
    let out = hdlsim()
        .args(["check", "--error-format", "json"])
        .arg(&f)
        .output()
        .expect("runs");
    assert!(!out.status.success(), "syntax error must fail the check");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let json_lines: Vec<&str> = stderr.lines().filter(|l| l.starts_with('{')).collect();
    assert!(
        !json_lines.is_empty(),
        "expected JSON lines on stderr:\n{stderr}"
    );
    for l in &json_lines {
        // Every diagnostic line must be a self-contained JSON object.
        serde_json::from_str::<serde_json::Value>(l)
            .unwrap_or_else(|e| panic!("line is not valid JSON: {l} ({e})"));
    }
    let v: serde_json::Value = serde_json::from_str(json_lines[0]).unwrap();
    assert_eq!(v["code"], "E0301");
    assert_eq!(v["severity"], "error");
    assert_eq!(v["file"], f.display().to_string());
    assert_eq!(v["line"], 3);
    assert!(v["message"].as_str().unwrap().contains(';'));
    assert!(v["byte_start"].is_u64() && v["byte_end"].is_u64());
}

#[test]
fn error_format_default_stays_human() {
    let f = tmp("bad_syntax2.v", "module m;\n  wire x\nendmodule\n");
    let out = hdlsim().args(["check"]).arg(&f).output().expect("runs");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("error[E0301]:"), "{stderr}");
    assert!(!stderr.lines().any(|l| l.starts_with('{')), "{stderr}");
}

#[test]
fn error_format_rejects_unknown_value() {
    let f = tmp("bad_syntax3.v", "module m;\nendmodule\n");
    let out = hdlsim()
        .args(["--error-format", "bogus", "check"])
        .arg(&f)
        .output()
        .expect("runs");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("invalid value 'bogus' for '--error-format'"),
        "{stderr}"
    );
}

#[test]
fn vhdl_assert_failure_carries_source_location() {
    // tb_fail_fatal.vhd: the failing assert is on line 7.
    let out = hdlsim()
        .args([
            "run",
            "--top",
            "tb_fail_fatal",
            "tests/fixtures/tb_fail_fatal.vhd",
        ])
        .output()
        .expect("runs");
    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("tb_fail_fatal.vhd:7: assertion failed"),
        "{stdout}"
    );
    assert!(
        stdout.contains("FAILED — 1 test failure(s) (first at ")
            && stdout.contains("tb_fail_fatal.vhd:7)"),
        "{stdout}"
    );
}

#[test]
fn verilog_error_carries_source_location() {
    // tb_fail_error.v: the $error call is on line 3.
    let out = hdlsim()
        .args([
            "run",
            "--top",
            "tb_fail_error",
            "tests/fixtures/tb_fail_error.v",
        ])
        .output()
        .expect("runs");
    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("tb_fail_error.v:3: error:"), "{stdout}");
    assert!(stdout.contains("tb_fail_error.v:3)"), "{stdout}");
}

#[test]
fn passing_run_has_no_failure_location() {
    let out = hdlsim()
        .args(["run", "--top", "tb_pass", "tests/fixtures/tb_pass.v"])
        .output()
        .expect("runs");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("FAILED"), "{stdout}");
}
