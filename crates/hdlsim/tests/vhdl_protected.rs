//! Regression test for VHDL-2008 protected types: the protected body's state
//! variables become fields of a simulator object (64-bit handle model shared
//! with SV classes), method bodies lower as `t::m` functions with bare state
//! references mapped to `$getf`/`$setf` on `$this`, and `obj.m(...)` calls
//! dispatch via `$method::`/`$callmethod::`. Objects (`shared variable x : t;`)
//! are allocated and initialized by a generated initial process (`$new::t`
//! runs the generated `t::new` ctor with the field initializers).
//! Previously methods were stubs and calls returned X.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

#[test]
fn protected_type_state_and_methods_simulate() {
    let f = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vhdl_protected_tb.vhd");
    let out = hdlsim()
        .args(["run", "--top", "tb_protected"])
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
    assert!(stdout.contains("PASS: vhdl protected types"), "{}", stdout);
    assert!(
        !stderr.contains("unknown identifier"),
        "protected bodies must lower cleanly: {}",
        stderr
    );
}

#[test]
fn protected_type_cross_file_simulates() {
    // Package and testbench in separate files: `use work.prot_pkg.all;` must
    // expose the protected type's fields/methods (PackageIr::prot_tys), so
    // `sv.inc`/`sv.get` dispatch to the real `counter_t::m` bodies.
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let pkg = dir.join("vhdl_protected_pkg.vhd");
    let tb = dir.join("vhdl_protected_xfile_tb.vhd");
    let out = hdlsim()
        .args(["run", "--top", "tb_protected_xfile"])
        .arg(&pkg)
        .arg(&tb)
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
        stdout.contains("PASS: vhdl protected types cross-file"),
        "{}",
        stdout
    );
}
