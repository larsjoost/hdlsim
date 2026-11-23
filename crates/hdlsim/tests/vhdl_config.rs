//! VHDL configurations: block configurations inside a configuration
//! declaration (applied when the configuration is instantiated or is the
//! top), configuration specifications in declarative parts
//! (`for label : comp use entity ...;`), `for all` / generic maps in
//! bindings, and instantiation by `configuration work.cfg`.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

#[test]
fn configurations_simulate() {
    let f = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vhdl_config_tb.vhd");
    let out = hdlsim()
        .args(["run", "--top", "tb_config"])
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
    assert!(stdout.contains("PASS: vhdl configurations"), "{}", stdout);
}
