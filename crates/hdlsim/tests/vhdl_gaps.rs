//! Regression tests for matrix gaps: multi-dimensional arrays
//! (`array (0 to 1, 0 to 3) of ...`, `m(i, j)` flat indexing), static
//! attributes (`'length`/`'high`/`'low`/`'left`/`'right`), and concurrent
//! `assert` (previously warned "not lowered yet").

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
fn multidim_arrays_attrs_and_concurrent_assert_simulate() {
    let f = fixture("vhdl_gaps_tb.vhd");
    let out = hdlsim()
        .args(["run", "--top", "tb_gaps"])
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
    assert!(stdout.contains("PASS: vhdl gaps"), "{}", stdout);
    assert!(
        !stderr.contains("not lowered yet"),
        "concurrent assert must be lowered: {}",
        stderr
    );
}
