//! Regression test for simulated std.textio: `write`/`writeline` to `output`,
//! and `readline`/`read`/`endfile` on a real file object. The fixture's data
//! path is relative to the crates/hdlsim directory (the test's CWD).

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.env("HDLSIM_NO_CACHE", "1");
    c
}

#[test]
fn textio_write_writeline_readline_read() {
    let f = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vhdl_textio_tb.vhd");
    let out = hdlsim()
        .args(["run", "--top", "tb_textio"])
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
    assert!(stdout.contains("W=16 V=255"), "{}", stdout);
    assert!(stdout.contains("N=7 W=hello"), "{}", stdout);
    assert!(stdout.contains("PASS: vhdl textio"), "{}", stdout);
    assert!(!stdout.contains("assertion failed"), "{}", stdout);
}

#[test]
fn check_accepts_textio_and_file_declarations() {
    let f = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vhdl_textio_tb.vhd");
    let out = hdlsim().args(["check"]).arg(&f).output().expect("runs");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "expected exit 0, got {:?}\n{}",
        out.status,
        stderr
    );
    assert!(!stderr.contains("error["), "{}", stderr);
}
