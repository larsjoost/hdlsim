//! Regression test: a VHDL signal or variable initializer may be a bare
//! name (constant reference). Declaration lowering used to skip Name nodes
//! when looking for the initializer expression, so `SIGNAL s : natural :=
//! c_width;` silently lost its initializer and the signal took the default
//! value instead.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.env("HDLSIM_NO_CACHE", "1");
    c
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/{}", name))
}

/// The testbench asserts in-band that every initializer held; a lost
/// initializer fails an assert and the run exits 1.
#[test]
fn bare_name_initializers_reach_signal_and_variable() {
    let out = hdlsim()
        .args(["run", "--top", "tb_init_name"])
        .arg(fixture("signal_init_name.vhd"))
        .output()
        .expect("runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "initializers must hold, run said:\n{stdout}"
    );
    assert!(!stdout.contains("FAILED"), "{stdout}");
}
