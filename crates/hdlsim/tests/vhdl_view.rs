//! Regression test for VHDL-2019 interface views: `view` declarations parse
//! and register the view name as a type alias of its subtype, `view V of T`
//! interface elements lower as ports of type `T`, `V'converse` aliases
//! resolve, external-name aliases (`<< signal ^.u0.sig : t >>`) lower without
//! E0401, an architecture inherits its entity's context clauses and
//! declarative items when they live in separate files (LRM 1076 §13.3), and
//! record aliases with a `field'subtype` subtype support field selects.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.env("HDLSIM_NO_CACHE", "1");
    c
}

#[test]
fn vhdl2019_views_and_entity_context_simulate() {
    let f = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vhdl_view_tb.vhd");
    let out = hdlsim()
        .args(["run", "--top", "tb_view"])
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
        stdout.contains("PASS: vhdl2019 views, record aliases, entity context"),
        "{}",
        stdout
    );
    assert!(
        !stderr.contains("unknown identifier") && !stderr.contains("error["),
        "must lower cleanly: {}",
        stderr
    );
}
