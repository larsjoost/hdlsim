//! Regression test for runtime indexing of constant arrays (including arrays
//! of records, `c(i).field`, arch-level and function-local) and for array
//! signal initializers being applied at simulation start.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

#[test]
fn const_record_arrays_and_array_initializers() {
    let f =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vhdl_const_recarr_tb.vhd");
    let out = hdlsim()
        .args(["run", "--top", "tb_const_recarr"])
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
    assert!(stdout.contains("w=33 m=add"), "{}", stdout);
    assert!(
        stdout.contains("PASS: vhdl const record arrays"),
        "{}",
        stdout
    );
    assert!(!stdout.contains("FAIL"), "{}", stdout);
    assert!(!stderr.contains("error["), "{}", stderr);
}
