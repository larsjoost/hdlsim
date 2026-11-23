//! Regression test for unconstrained `range <>` array indices beyond depth 1:
//! `type mem_t is array (range <>, range <>) of ...` previously failed to
//! parse in a multi-index list (`expected ')' to close the constraint`).

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

#[test]
fn multi_dim_box_index_array_simulates() {
    let f = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vhdl_boxarray_tb.vhd");
    let out = hdlsim()
        .args(["run", "--top", "tb_boxarray"])
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
    assert!(stdout.contains("PASS: vhdl box array"), "{}", stdout);
}
