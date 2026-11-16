//! Regression test for architecture selection: instantiation with an explicit
//! architecture `entity work.dual(rtl)` and configuration-driven default
//! binding (`configuration cfg of dual2 is for rtl …`). Previously all
//! architectures merged onto the entity's single module and the selector was
//! dropped.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.env("HDLSIM_NO_CACHE", "1");
    c
}

#[test]
fn architecture_selection_simulates() {
    let f =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vhdl_arch_select_tb.vhd");
    let out = hdlsim()
        .args(["run", "--top", "tb_arch_select"])
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
    assert!(stdout.contains("PASS: vhdl arch select"), "{}", stdout);
}
