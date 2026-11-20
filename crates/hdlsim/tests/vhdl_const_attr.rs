//! Regression test for const-evaluated attributes and conversions in constant
//! initializers: `type'val(x)`, `type'pos(x)`, `T'image(x)`, `to_integer(x)`.
//! These lowered to Expr::Call/ToInt/ToString forms the const-evaluator
//! rejected, so the constant silently became X with a W0401 warning.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.env("HDLSIM_NO_CACHE", "1");
    c
}

#[test]
fn const_initializers_with_val_pos_image_to_integer() {
    let f = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vhdl_const_attr_tb.vhd");
    let out = hdlsim()
        .args(["run", "--top", "tb_const_attr"])
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
        stdout.contains("PASS: vhdl const attr/val/image"),
        "{}",
        stdout
    );
    assert!(
        !stderr.contains("constant initializer is not constant"),
        "initializers must const-eval: {}",
        stderr
    );
}
