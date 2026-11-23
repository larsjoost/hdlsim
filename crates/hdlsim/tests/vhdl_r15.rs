//! Regression test for the VHDL-2019 conditional expression with an omitted
//! final `else` (`v when c` — LRM 1076-2019 §9.2.9; the missing else branch
//! means 'others => X'), needed by OSVVM's SPI_GuyEschemann model library
//! (`retval := '1' when SpiMode = 2 or SpiMode = 3;` in SpiTbPkg.vhd).
//! Also covers the 2019 chained form `v when c1 else w when c2`, where the
//! else branch is itself a conditional expression.
//!
//! The same fixture exercises user-declared physical types
//! (`type dist_t is range ... units angstrom; nm = 10 angstrom; ... end units;`):
//! physical literals with custom units (`1.5 cm`) and bare unit names (`mm`)
//! lower to base-unit integers (needed by `ghdl4_sanity_004all08_all08.vhdl`:
//! `d := 1.5 cm when v >= 0 else mm;`).

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

#[test]
fn vhdl_2019_cond_expr_without_final_else() {
    let f = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vhdl_r15_tb.vhd");
    let out = hdlsim()
        .args(["run", "--top", "tb_r15"])
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
        stdout.contains("PASS: vhdl-2019 conditional expression without final else"),
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
