//! Regression test for the corpus round 19 front-end fixes driven by the
//! VHDL/Compliance-Tests `vhdl_2008/` + `vhdl_2019/` files still at `diags`
//! after they were added to the corpus: the VHDL-2008 matching case with the
//! `end case?` form, the VHDL-2019 sequential block statement, the subprogram
//! generic clause with `a'subtype` formal/return marks, the `return rv_t of T`
//! return form, and the `U` bit-string literal prefix.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

#[test]
fn vhdl_r19_case_q_seq_block_subprog_generic_return_of_u_bitstring() {
    let f = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vhdl_r19_tb.vhd");
    let out = hdlsim()
        .args(["run", "--top", "tb_r19"])
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
        stdout.contains("PASS: round-19 case?/seq-block/subprog-generic/return-of/U-bitstring"),
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
