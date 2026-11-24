//! Regression test for the corpus round 20 front-end fix driven by
//! UVVM's `uvvm_util` sources: a subtype declared in a protected-type
//! body's declarative part (`subtype t_data_buffer is std_logic_vector(...)`,
//! as in `data_queue_pkg`) must be registered before the state variable
//! that uses it — the protected-body lowering pass previously skipped
//! `SubtypeDecl`, so `variable priv_data_buffer : t_data_buffer;` failed
//! with "unknown type".

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

#[test]
fn vhdl_r20_protected_body_local_subtype() {
    let f = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vhdl_r20_tb.vhd");
    let out = hdlsim()
        .args(["run", "--top", "tb_r20"])
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
        stdout.contains("PASS: round-20 protected-body local subtype"),
        "{}",
        stdout
    );
    assert!(!stdout.contains("FAIL"), "{}", stdout);
    assert!(
        !stderr.contains("error["),
        "must lower without errors: {}",
        stderr
    );
}
