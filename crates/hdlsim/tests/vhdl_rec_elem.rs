//! Regression test: a record-element constraint on a subprogram-local
//! variable (`v : node_t(name(1 to n))`) must not lower the element name as
//! an identifier — it regressed to `error[E0401]: unknown identifier 'name'`
//! when the dyn-width machinery started lowering non-folding constraints
//! (caught by the third-party corpus baseline). A string-typed local with a
//! formal-dependent range keeps folding in the same body.

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
fn record_element_constraint_compiles_clean() {
    let f = fixture("vhdl_rec_elem_tb.vhd");
    let out = hdlsim()
        .args(["run", "--top", "rec_elem_tb"])
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
    // The record built through the element-constrained local folds.
    assert!(stdout.contains("NODE_VALID=true"), "{}", stdout);
    // No error diagnostics from constraint lowering. (The runtime VALUE of a
    // formal-dependent string local's slice assigns has a pre-existing
    // packing gap — out of scope here; the corpus only tracks diagnostics.)
    assert!(!stderr.contains("E0401"), "{}", stderr);
}
