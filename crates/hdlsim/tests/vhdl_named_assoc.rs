//! Regression tests for association-list lowering: the lexer emits the `=>`
//! arrow as two punctuation tokens, so the lowerer must recognise the
//! adjacent pair — otherwise named generic/port associations degrade to
//! positional, the generic actuals leak into the port connections and shift
//! them. Also pins the fail-closed rejection of positional generic map
//! associations.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.env("HDLSIM_NO_CACHE", "1");
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
fn positional_generic_map_rejected() {
    let out = hdlsim()
        .args(["run", "--top", "tb_positional_generic"])
        .arg(fixture("vhdl_named_assoc_positional_tb.vhd"))
        .output()
        .expect("runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "expected failure, got success\n{}{}",
        stdout,
        stderr
    );
    assert!(
        stderr.contains("positional generic map"),
        "expected the positional-generic-map diagnostic, got:\n{}",
        stderr
    );
}
