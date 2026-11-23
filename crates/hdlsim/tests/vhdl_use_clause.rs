//! Regression test for cross-file package visibility: `use work.mypkg.all;`
//! must make the package's types visible in another file (the package is
//! resolved on demand from the library, lowered, and merged into scope).
//! Also covers the embedded ieee.std_logic_1164 via its use clause.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

#[test]
fn use_work_package_makes_types_visible() {
    let dir = fixtures().join("pkg_use");
    let out = hdlsim()
        .args(["check", "-L"])
        .arg(format!("work={}", dir.display()))
        .arg(dir.join("top.vhd"))
        .output()
        .expect("runs");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "expected exit 0, got {:?}\n{}{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        stderr
    );
    assert!(!stderr.contains("E0401"), "{}", stderr);
}

#[test]
fn use_ieee_std_logic_1164_via_embedded_stdlib() {
    let f = fixtures().join("std_ulogic_use.vhd");
    let out = hdlsim().arg("check").arg(&f).output().expect("runs");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "expected exit 0, got {:?}\n{}",
        out.status,
        stderr
    );
    assert!(!stderr.contains("E0401"), "{}", stderr);
}
