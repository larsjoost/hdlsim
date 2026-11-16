//! Regression test for the reported `architecture for unknown entity` error:
//! the caret must point at the entity name (not the whole architecture line),
//! a `did you mean` help must suggest the similarly named entity from the
//! other file, and the rustc-style summary must end with a newline so the
//! report hint is not glued onto it.

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

#[test]
fn unknown_entity_error_points_at_name_and_ends_with_newline() {
    let entity = fixture("arch_unknown/edge_detector_entity.vhd");
    let rtl = fixture("arch_unknown/edge_detector_rtl.vhd");
    let out = hdlsim()
        .arg("check")
        .arg(&entity)
        .arg(&rtl)
        .output()
        .expect("runs");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("architecture for unknown entity 'edge_detector'"),
        "{}",
        stderr
    );

    // The caret must underline exactly the entity name on the architecture
    // line (same column, same width) — not the whole line.
    let src_line = stderr
        .lines()
        .find(|l| l.contains("ARCHITECTURE rtl OF edge_detector IS"))
        .unwrap_or_else(|| panic!("source line missing:\n{}", stderr));
    let caret_line = stderr
        .lines()
        .find(|l| l.contains('^'))
        .unwrap_or_else(|| panic!("caret line missing:\n{}", stderr));
    let name_col = src_line.find("edge_detector").unwrap() - src_line.find('|').unwrap();
    let caret_col = caret_line.find('^').unwrap() - caret_line.find('|').unwrap();
    assert_eq!(name_col, caret_col, "caret not under the name:\n{}", stderr);
    assert_eq!(
        caret_line.chars().filter(|c| *c == '^').count(),
        "edge_detector".len(),
        "caret width:\n{}",
        stderr
    );

    // The `-->` header points at the name's line:column.
    let rtl_src = std::fs::read_to_string(&rtl).unwrap();
    let (line_no, line) = rtl_src
        .lines()
        .enumerate()
        .find(|(_, l)| l.contains("OF edge_detector"))
        .unwrap();
    let col = line.find("edge_detector").unwrap() + 1;
    assert!(
        stderr.contains(&format!(":{}:{}", line_no + 1, col)),
        "expected location {}:{}:\n{}",
        line_no + 1,
        col,
        stderr
    );

    // Did-you-mean help suggests the entity declared in the other file.
    assert!(
        stderr.contains("did you mean 'edge_detector_entity'?"),
        "{}",
        stderr
    );

    // The summary line ends with a newline: the report hint starts on its
    // own line instead of being glued onto `... previous error`.
    assert!(
        stderr.contains("error: could not compile due to 1 previous error\n"),
        "{}",
        stderr
    );
    assert!(
        stderr.contains("previous error\nnote: hdlsim"),
        "summary not newline-terminated:\n{}",
        stderr
    );
}
