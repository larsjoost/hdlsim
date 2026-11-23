//! Regression test for the VHDL-2008/2019 constructs used by OSVVM:
//! std.standard additions (integer_vector, file_open_kind/status, char
//! literals, `now`), generic packages with type parameters, incomplete and
//! protected types, subprogram aliases with signatures, function-local
//! constants referencing parameters, record fields on parameters.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

#[test]
fn osvvmish_constructs() {
    let f = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vhdl_osvvmish_tb.vhd");
    let out = hdlsim()
        .args(["run", "--top", "tb_osvvmish"])
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
        stdout.contains("PASS: vhdl osvvmish constructs"),
        "{}",
        stdout
    );
    assert!(!stdout.contains("FAIL"), "{}", stdout);
    assert!(!stderr.contains("error["), "{}", stderr);
}
