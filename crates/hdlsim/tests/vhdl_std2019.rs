//! Regression test for the embedded VHDL-2008/2019 standard units:
//! `context ieee.ieee_std_context;` (embedded `ieee.std_context` context
//! declaration), `ieee.numeric_std_unsigned` (transitive re-export of
//! ieee.numeric_std plus `to_slv`), and `ieee_proposed.standard_additions`
//! (decl-only `to_*` family; `to_string` of a real formats as decimal).
//! All three used to warn W0501 (unresolved library unit) / misbehave.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.env("HDLSIM_NO_CACHE", "1");
    c
}

#[test]
fn std_context_numeric_std_unsigned_standard_additions_resolve() {
    let f = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vhdl_std2019_tb.vhd");
    let out = hdlsim()
        .args(["run", "--top", "tb_std2019"])
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
        stdout.contains("PASS: vhdl std_context/numeric_std_unsigned/standard_additions"),
        "{}",
        stdout
    );
    assert!(
        !stderr.contains("W0501"),
        "standard units must resolve without W0501: {}",
        stderr
    );
}
