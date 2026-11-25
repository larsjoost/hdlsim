//! End-to-end tests for `--dump-consts`: every constant/generic prints as
//! the lowerer binds it — scope, kind, source line, fold status, value —
//! in text form, or as JSON Lines with `--error-format json`. This is the
//! elaboration-time view behind W0401 cascades: the first X constant
//! printed above a failure is the root.

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

/// The `cd …` records among the stderr lines (the dump's text prefix, and
/// the JSON object key with `--error-format json`... which emits `{"cd":`,
/// filtered by its own test).
fn dump_lines(stderr: &str) -> Vec<&str> {
    stderr.lines().filter(|l| l.starts_with("cd ")).collect()
}

#[test]
fn dump_consts_text() {
    let out = hdlsim()
        .args(["run", "--dump-consts", "--top", "dmp_tb"])
        .arg(fixture("vhdl_dump_consts_tb.vhd"))
        .output()
        .expect("runs");
    assert!(
        out.status.success(),
        "expected exit 0: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    let lines = dump_lines(&stderr);
    // Off by default: no records without the flag.
    let quiet = hdlsim()
        .args(["run", "--top", "dmp_tb"])
        .arg(fixture("vhdl_dump_consts_tb.vhd"))
        .output()
        .expect("runs");
    assert!(dump_lines(&String::from_utf8_lossy(&quiet.stderr)).is_empty());
    // Known constant: scope, kind, width, status, hex value.
    assert!(
        lines
            .iter()
            .any(|l| l.contains("cd dmp_pkg.C_ANSWER kind=constant width=32 status=ok value=0x2a")),
        "{lines:?}"
    );
    // String constant: byte-sized with an ASCII rendering.
    assert!(
        lines.iter().any(|l| l
            .contains("cd dmp_pkg.C_STR kind=constant width=16 status=ok value=0x6f6b")
            && l.contains("ascii=\"ok\"")),
        "{lines:?}"
    );
    // Source line of the declaration.
    assert!(
        lines
            .iter()
            .any(|l| l.contains("C_ANSWER") && l.contains("vhdl_dump_consts_tb.vhd:10")),
        "{lines:?}"
    );
    // Generic override and default, one record per variant re-lowering.
    assert!(
        lines.iter().any(|l| l.contains("dmp_unit.G_W")
            && l.contains("kind=generic")
            && l.contains("value=0x10")),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.contains("dmp_unit.G_W")
            && l.contains("kind=generic")
            && l.contains("value=0x8")),
        "{lines:?}"
    );
    // Generic with no default and no override: unset, not silently ok.
    assert!(
        lines
            .iter()
            .any(|l| l.contains("dmp_unit.G_X") && l.contains("status=unset")),
        "{lines:?}"
    );
    // Real `**` folds like every other real operator; the folded value is
    // the real sqrt(2).
    assert!(
        lines.iter().any(|l| l.contains("dmp_unit/rtl.C_ROOT2")
            && l.contains("status=ok")
            && l.contains("1.4142135623730951")),
        "{lines:?}"
    );
    // A genuinely unfoldable initializer (procedure call) stays unfolded.
    assert!(
        lines
            .iter()
            .any(|l| l.contains("dmp_unit/rtl.C_UNFOLDABLE") && l.contains("status=unfolded")),
        "{lines:?}"
    );
}

#[test]
fn dump_consts_json() {
    let out = hdlsim()
        .args([
            "run",
            "--dump-consts",
            "--error-format",
            "json",
            "--top",
            "dmp_tb",
        ])
        .arg(fixture("vhdl_dump_consts_tb.vhd"))
        .output()
        .expect("runs");
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    let lines: Vec<&str> = stderr
        .lines()
        .filter(|l| l.starts_with("{\"cd\":"))
        .collect();
    assert!(!lines.is_empty(), "{}", stderr);
    assert!(
        lines.iter().any(|l| l.contains("\"name\":\"C_ANSWER\"")
            && l.contains("\"kind\":\"constant\"")
            && l.contains("\"status\":\"ok\"")
            && l.contains("\"value\":\"0x2a\"")
            && l.contains("\"line\":10")),
        "{lines:?}"
    );
}
