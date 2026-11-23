//! Regression test for instantiation-time `generic map` binding: a child
//! entity's generics (port widths, for-generate ranges) must be re-bound at
//! elaboration via an elab-time variant re-lower, not just folded from the
//! declared defaults. Previously `generic map (WIDTH => 8)` had no effect on
//! the child and a generic without default left its for-generate skipped.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

#[test]
fn generic_map_rebinds_child_generics() {
    let f =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vhdl_generic_map_tb.vhd");
    let out = hdlsim()
        .args(["run", "--top", "tb_generic_map"])
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
        stdout.contains("PASS: vhdl generic map binding"),
        "{}",
        stdout
    );
}
