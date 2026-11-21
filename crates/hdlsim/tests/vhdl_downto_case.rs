//! Regression test (PR #132 follow-up): range direction is decided by the
//! `downto`/`to` keyword, and VHDL keywords are case-insensitive. A
//! case-sensitive keyword match read `3 DOWNTO 0` as ascending (low=3,
//! high=0), sized every descending range as a null range (width/depth 1),
//! and made slice writes land at huge wrapped indexes.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.env("HDLSIM_NO_CACHE", "1");
    c
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/{}", name))
}

/// `v : std_ulogic_vector(3 DOWNTO 0)` must be 4 bits wide and
/// `a : t_arr` with `ARRAY (3 DOWNTO 0) OF std_ulogic` must be 4 words
/// deep — for both keyword spellings (`DOWNTO` and `downto`).
#[test]
fn descending_range_width_is_case_insensitive() {
    let out = hdlsim()
        .args(["debug", "--top", "tb_downto_case", "--dump"])
        .arg(fixture("downto_case.vhd"))
        .output()
        .expect("runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{}", stdout);
    for sig in ["v_upper", "v_lower"] {
        let line = stdout
            .lines()
            .find(|l| l.split_whitespace().any(|t| t == sig))
            .unwrap_or("");
        assert!(
            line.contains("w=4"),
            "{sig} must be width 4, dump line: '{line}'\n{stdout}"
        );
    }
    for sig in ["a_upper", "a_lower"] {
        let line = stdout
            .lines()
            .find(|l| l.split_whitespace().any(|t| t == sig))
            .unwrap_or("");
        assert!(
            line.contains("mem[0..4]"),
            "{sig} must be a 4-word memory, dump line: '{line}'\n{stdout}"
        );
    }
}
