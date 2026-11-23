//! Regression test for VHDL-2008 generic packages and package instantiation:
//! `package p is new g generic map (X => v);` at design-unit level and in an
//! architecture declarative part, `generic (...)` clauses in package
//! declarations, and same-file `use work.pkg.all;`.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

fn run_fixture(fixture: &str, top: &str, pass_msg: &str) {
    let f = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(fixture);
    let out = hdlsim()
        .args(["run", "--top", top])
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
    assert!(stdout.contains(pass_msg), "{}{}", stdout, stderr);
    assert!(!stdout.contains("assertion failed"), "{}", stdout);
}

#[test]
fn generic_package_instantiation_at_design_unit_level() {
    run_fixture(
        "vhdl_genpkg_tb.vhd",
        "tb_genpkg",
        "PASS: vhdl generic packages",
    );
}

#[test]
fn generic_package_instantiation_in_architecture() {
    run_fixture(
        "vhdl_genpkg_arch_tb.vhd",
        "tb_genpkg_arch",
        "PASS: vhdl generic packages (arch-level)",
    );
}

#[test]
fn check_accepts_generic_packages_and_same_file_use() {
    let f = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vhdl_genpkg_tb.vhd");
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
