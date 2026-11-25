//! Regression test: an `if` over an unknown condition takes the else path in
//! const-eval, matching the runtime (VHDL LRM 1076 §10.8 — only TRUE runs the
//! sequence; Verilog: x/z selects else). A constant whose initializer's only
//! unknown is an unbound formal generic folds in the bare-module context to
//! the else value with no warning, and to the real value per instance.

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

#[test]
fn unknown_if_condition_takes_else_path_when_const_evaluating() {
    let f = fixture("vhdl_cond_if_x_tb.vhd");

    // Bare-module check: the fold succeeds via the else path — no warning.
    let out = hdlsim().args(["check"]).arg(&f).output().expect("runs");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "expected exit 0, got {:?}\n{}{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        stderr
    );
    assert!(!stderr.contains("W0401"), "{}", stderr);

    // The bare fold took the else path (7), not X.
    let dump = hdlsim()
        .args(["check", "--dump-consts"])
        .arg(&f)
        .output()
        .expect("runs");
    let dump_all = format!(
        "{}{}",
        String::from_utf8_lossy(&dump.stdout),
        String::from_utf8_lossy(&dump.stderr)
    );
    let bare = dump_all
        .lines()
        .find(|l| l.contains("cond_blk/rtl.C_X"))
        .expect("bare context dump line");
    assert!(bare.contains("status=ok value=0x7"), "{}", bare);

    // Per instance (generic map G_W => 16) the condition is true: C_X = 1.
    let run = hdlsim()
        .args(["run", "--top", "cond_if_x_tb"])
        .arg(&f)
        .output()
        .expect("runs");
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(run.status.success(), "{}", stdout);
    assert!(stdout.contains("CX=1"), "{}", stdout);
}
