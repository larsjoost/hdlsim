//! Regression test: a constant initializer whose only failure is an unbound
//! formal generic (required, no default) must not warn in the bare-module
//! context — it is constant per instance, where the generic map rebinds it.
//! The same initializer with a plain argument must keep warning, so the
//! suppression stays narrow.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/{}", name))
}

#[test]
fn unbound_generic_constant_initializer_does_not_warn_but_plain_one_does() {
    let f = fixture("vhdl_unbound_generic_tb.vhd");
    let out = hdlsim().args(["check"]).arg(&f).output().expect("runs");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "expected exit 0, got {:?}\n{}{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        stderr
    );
    // The generic-dependent constant (C_W) is silent; the plain one (C_PLN)
    // still warns.
    assert_eq!(stderr.matches("W0401").count(), 1, "{}", stderr);
    assert!(stderr.contains("C_PLN"), "{}", stderr);
    assert!(!stderr.contains("C_W"), "{}", stderr);
}
