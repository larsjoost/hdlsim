//! Regression tests for association-list lowering: the lexer emits the `=>`
//! arrow as two punctuation tokens, so the lowerer must recognise the
//! adjacent pair — otherwise named generic/port associations degrade to
//! positional, the generic actuals leak into the port connections and shift
//! them. Positional generic maps must bind to the entity's generics in
//! declaration order (they were first mis-bound via the port path, then
//! rejected outright, before getting their own resolution pass).

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn named_assoc_maps_bind() {
    let out = hdlsim()
        .args(["run", "--top", "tb_named_assoc"])
        .arg(fixture("vhdl_named_assoc_tb.vhd"))
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
        stdout.contains("PASS: named generic/port associations bind correctly"),
        "{}",
        stdout
    );
}

#[test]
fn positional_generic_map_binds_in_order() {
    let out = hdlsim()
        .args(["run", "--top", "tb_positional_generic"])
        .arg(fixture("vhdl_named_assoc_positional_tb.vhd"))
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
        stdout.contains("PASS: positional and named generic maps bind correctly"),
        "{}",
        stdout
    );
}
