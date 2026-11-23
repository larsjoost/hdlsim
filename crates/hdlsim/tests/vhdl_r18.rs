//! Regression test for the corpus round 18 front-end fixes driven by the
//! GHDL testsuite `synth/` files still at `diags` after round 17:
//! VHDL-2008 `unaffected` waveform elements (asgn01/asgn10,
//! issue2361/jkff), the box generic `T : type` (issue412/generic_sfifo),
//! external-name aliases with an absolute path (external01/external03), and
//! the VHDL-2019 `math_real` constants such as `MATH_SQRT_PI`
//! (issue2176/math_real_test).

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

#[test]
fn vhdl_r18_unaffected_box_generic_extname_math_real() {
    let f = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vhdl_r18_tb.vhd");
    let out = hdlsim()
        .args(["run", "--top", "tb_r18"])
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
        stdout.contains("PASS: round-18 unaffected/box-generic/extname/math_real"),
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
