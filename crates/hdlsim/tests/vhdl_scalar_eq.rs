//! Regression tests for predefined (in)equality (LRM 1076, relational
//! operators): the result is a boolean value comparison, never the
//! X-propagating variant — `'U' = '0'` is false, `'U' /= '0'` is true, so a
//! guarded wait whose condition reads `sig /= '0'` while the signal is
//! still uninitialized must be taken.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

fn run(top: &str, file: &str, extra: &[&str]) -> (std::process::Output, String, String) {
    let f = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(file);
    let out = hdlsim()
        .args(["run", "--top", top])
        .args(extra)
        .arg(&f)
        .output()
        .expect("hdlsim binary runs");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    (out, stdout, stderr)
}

#[test]
fn scalar_eq_on_uninitialized_is_false_not_x() {
    let (out, stdout, stderr) = run(
        "tb_scalar_eq",
        "tests/fixtures/vhdl_scalar_eq_tb.vhd",
        &["--max-time", "1us"],
    );
    assert!(
        out.status.success(),
        "expected exit 0, got {:?}\n{}{}",
        out.status,
        stdout,
        stderr
    );
    assert!(stdout.contains("tb_scalar_eq: PASS"), "{}", stdout);
    // The t=0 comparison results must be booleans, not X.
    assert!(
        stdout.contains("(sig_a = '0')  is false"),
        "equality must compare values: {}",
        stdout
    );
    assert!(
        stdout.contains("(sig_a /= '0') is true"),
        "inequality must compare values: {}",
        stdout
    );
}
