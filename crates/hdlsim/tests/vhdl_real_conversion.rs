//! Regression test: `real(x)` is a value conversion. Lowering it as a
//! bit-identical passthrough made an integer-argument overload whose body
//! calls the real-argument overload via `real(x)` call itself with the same
//! integer argument — infinite recursion stopped only by the call-depth
//! guard (`function call depth exceeded`).

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

/// The overload chain must terminate: scaled(2) reaches the real-argument
/// overload through the `real(x)` value conversion and yields 22.
#[test]
fn real_conversion_reaches_the_real_overload() {
    let out = hdlsim()
        .args(["run", "--top", "tb_real_conv"])
        .arg(fixture("real_conv_pkg.vhd"))
        .arg(fixture("real_conv_tb.vhd"))
        .output()
        .expect("runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "conv chain must terminate with 22, run said:\n{stdout}"
    );
    assert!(!stdout.contains("FAILED"), "{stdout}");
    assert!(!stdout.contains("depth exceeded"), "{stdout}");
}
