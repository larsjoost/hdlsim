//! Regression test for the reported parse error on
//! `END ARCHITECTURE behavior;` — the parser accepted `end;`, `end rtl;` and
//! `end architecture;` but not the keyword + name form `end architecture rtl;`,
//! because it bumped `architecture` as the optional name and then expected `;`.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

#[test]
fn end_architecture_keyword_and_name_parses() {
    let f =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/end_architecture_name.vhd");
    let out = hdlsim().arg("check").arg(&f).output().expect("runs");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "expected exit 0, got {:?}\n{}{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        stderr
    );
    assert!(!stderr.contains("E0301"), "{}", stderr);
}
