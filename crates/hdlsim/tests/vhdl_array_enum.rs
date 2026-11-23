//! Regression test for the reported `error[E0401]: unknown identifier
//! 'device_under_test_t'` on `TYPE state_t IS ARRAY (device_under_test_t) OF
//! std_ulogic;` — an array whose index is an enum *type mark* (parsed as a
//! Range containing a Name). The lowerer must resolve the depth from the
//! enum's literal count, not lower the type name as an expression.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

#[test]
fn array_type_with_enum_index_checks_clean() {
    let f = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/array_enum_index.vhd");
    let out = hdlsim().arg("check").arg(&f).output().expect("runs");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "expected exit 0, got {:?}\n{}{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        stderr
    );
    assert!(!stderr.contains("E0401"), "{}", stderr);
    assert!(!stderr.contains("unknown identifier"), "{}", stderr);
}
