//! Regression: positional port map entries were silently dropped by the
//! lowerer (treated as named associations with no actual), leaving child
//! ports unconnected — the DUT never saw its clock and outputs floated.

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

#[test]
fn positional_port_map_connects() {
    let out = hdlsim()
        .args(["run", "--top", "tb_posmap", "--max-time", "100ns"])
        .arg(fixture("vhdl_pos_port_map_tb.vhd"))
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
    assert!(stdout.contains("PASS: positional port map"), "{}", stdout);
}
