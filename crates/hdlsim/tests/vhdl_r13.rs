//! Regression test for the VHDL constructs OSVVM's model libraries need
//! (corpus round 13):
//! - entity declarative-region items are visible in every architecture of the
//!   entity: entity-level `signal` declarations (incl. record subtypes with
//!   field constraints like `RecType(f(7 downto 0))`) and entity-level
//!   `alias` declarations (`alias a : T is port.field;`) resolve in
//!   architectures (LRM 1076 §1.2).
//! - VHDL-2019 mode-view ports (`port (bus : view V of T)`): member selects
//!   (`bus.elem.field`), `bus.elem'subtype` type marks, and `'length` on them
//!   resolve statically.
//! - Cross-library context references (`context lib.ctx;`), incl. nested
//!   context references inside the referenced context declaration.
//! - Package instantiations naming a library-qualified generic package
//!   (`package p is new lib.g generic map (...)`), and selected paths into the
//!   instance (`p.sig`, `p.const`) with instantiation-specific widths.
//! - Non-constant waveform delays (`x <= v after sig/2`) lower to a warning
//!   (delay treated as 0) instead of an error that drops the assignment.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

#[test]
fn vhdl_r13_entity_decls_view_ports_pkg_insts() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vhdl_r13");
    let out = hdlsim()
        .args(["run", "--top", "tb"])
        .arg("-L")
        .arg(format!("r13lib={}", dir.join("lib").display()))
        .arg(dir.join("tb.vhd"))
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
        stdout.contains("PASS: r13 dut view ports + entity-level decls"),
        "{}",
        stdout
    );
    assert!(
        stdout.contains("PASS: r13 package instance selected path"),
        "{}",
        stdout
    );
    assert!(!stdout.contains("FAIL"), "{}", stdout);
    assert!(
        !stderr.contains("error["),
        "must lower without errors: {}",
        stderr
    );
    // The non-constant delay is a warning, not an error.
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("non-constant waveform delay"),
        "expected the delay warning: {}",
        combined
    );
}
