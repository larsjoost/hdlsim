//! Regression test for `alias` declarations: whole-signal, bit-index, and
//! slice aliases must lower (previously parsed but silently dropped, so
//! uses failed with unknown identifier).

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.env("HDLSIM_NO_CACHE", "1");
    c
}

#[test]
fn aliases_read_and_write_through() {
    let f = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vhdl_alias_tb.vhd");
    let out = hdlsim()
        .args(["run", "--top", "tb_alias"])
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
    assert!(stdout.contains("PASS: vhdl alias"), "{}", stdout);
    assert!(!stderr.contains("unknown identifier"), "{}", stderr);
}
