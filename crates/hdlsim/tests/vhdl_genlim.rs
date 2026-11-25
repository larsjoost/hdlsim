//! Regression test: a generate range hanging on an unbound formal generic —
//! directly, or through a constant that folded to X from one — must not warn
//! in the bare-module context (it is constant per instance), and must unfold
//! the loop per instance where the generic map rebinds the generic.

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
fn generate_range_on_unbound_generic_does_not_warn_but_folds_per_instance() {
    let f = fixture("vhdl_genlim_tb.vhd");

    // Bare-module check: the range hangs on LIM (X from the unbound G_W) —
    // the generate is skipped, silently.
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

    // Per instance (G_W => 2): LIM = 0, so exactly one generate iteration.
    let run = hdlsim()
        .args(["run", "--top", "genlim_tb"])
        .arg(&f)
        .output()
        .expect("runs");
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(run.status.success(), "{}", stdout);
    assert!(stdout.contains("GI=0"), "{}", stdout);
    assert!(!stdout.contains("GI=1"), "{}", stdout);
}
