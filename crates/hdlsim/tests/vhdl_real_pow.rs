//! Regression test: real `**` folds in a constant initializer like the other
//! real operators (the runtime already evaluated it; const-eval aborted with
//! a W0401 instead of folding).

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
fn real_pow_constant_initializer_folds() {
    let f = fixture("vhdl_real_pow_tb.vhd");
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

    let run = hdlsim()
        .args(["run", "--top", "real_pow_tb"])
        .arg(&f)
        .output()
        .expect("runs");
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(run.status.success(), "{}", stdout);
    assert!(stdout.contains("N=1024"), "{}", stdout);
}
