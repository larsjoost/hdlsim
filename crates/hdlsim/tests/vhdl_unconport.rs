//! Regression test for unconstrained array ports (`port (a : in
//! std_ulogic_vector)`): static attributes on them (`a'high`, `a'length`)
//! must lower to runtime `Expr::SigAttr` (the width is only known after
//! port connection) instead of folding to the placeholder width 1.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

#[test]
fn unconstrained_port_attributes_simulate() {
    let f = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vhdl_unconport_tb.vhd");
    let out = hdlsim()
        .args(["run", "--top", "tb_unconport"])
        .arg(&f)
        .output()
        .expect("runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "expected exit 0, got {:?}\n{}{}",
        out.status,
        stdout,
        stderr
    );
    assert!(
        stdout.contains("PASS: vhdl unconstrained port"),
        "{}",
        stdout
    );
}
