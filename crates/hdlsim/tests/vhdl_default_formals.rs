//! Regression test: VHDL formals may carry default values on the package-spec
//! interface; the body repeats the formals without them. A call omitting the
//! corresponding trailing actuals used to const-eval to X (const-eval picked
//! the spec stub / left the formal slot missing) and at runtime read X.

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

fn run_tb(files: &[&str]) -> (String, Option<i32>) {
    let out = hdlsim()
        .args(["run", "--top", "tb_def_formals"])
        .args(files.iter().map(|f| fixture(f)))
        .output()
        .expect("runs");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.code(),
    )
}

/// Spec and body in separate design units (the common library layout):
/// the spec's defaults must survive the spec-stub -> body replacement.
#[test]
fn omitted_defaulted_formals_fill_with_spec_separate_body() {
    let (stdout, code) = run_tb(&[
        "def_formals_spec.vhd",
        "def_formals_body.vhd",
        "def_formals_tb.vhd",
    ]);
    assert_eq!(code, Some(0), "{stdout}");
    assert!(!stdout.contains("FAILED"), "{stdout}");
}

/// Spec and body in one file: the body replaces the stub inside one lower
/// instance; defaults must carry over the same way.
#[test]
fn omitted_defaulted_formals_fill_with_combined_package() {
    let (stdout, code) = run_tb(&["def_formals_combined.vhd", "def_formals_tb.vhd"]);
    assert_eq!(code, Some(0), "{stdout}");
    assert!(!stdout.contains("FAILED"), "{stdout}");
}
