//! Regression tests for user-declared procedures (sequential subprograms
//! called as statements): syscalls and dynamic strings inside procedure
//! bodies must take effect — the pure function path drops them, so
//! procedures need the mutable execution path.
//!
//! Known limitation (pre-existing): signal assignment *to enclosing-scope
//! signals* from inside a procedure is not supported — subprogram bodies
//! use a small slot-id space that overlaps module signal ids, so such a
//! reference silently resolves to a frame slot. Procedures drive behaviour
//! through their parameters and syscalls today.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.env("HDLSIM_NO_CACHE", "1");
    c
}

fn run(top: &str, fixture: &str) -> std::process::Output {
    let f = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(fixture);
    hdlsim()
        .args(["run", "--top", top])
        .arg(&f)
        .output()
        .expect("runs")
}

#[test]
fn procedure_textio_file_truncates_on_reopen() {
    // A procedure-local `file f : text` plus file_open/write/writeline/
    // file_close: writes land in the opened path, and a second write_mode
    // open truncates — the file holds only the last message.
    let out_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/out");
    std::fs::create_dir_all(&out_dir).unwrap();
    let log = out_dir.join("proc_file_log.txt");
    let _ = std::fs::remove_file(&log);
    let out = run("tb_proc_file", "vhdl_proc_file_tb.vhd");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "expected exit 0, got {:?}\n{}{}",
        out.status,
        stdout,
        stderr
    );
    assert!(stdout.contains("PASS: vhdl proc file"), "{}", stdout);
    let content = std::fs::read_to_string(&log).expect("procedure wrote the log file");
    assert_eq!(content, "second\n", "write_mode open must truncate");
    std::fs::remove_file(&log).unwrap();
    let _ = std::fs::remove_dir(&out_dir);
}
