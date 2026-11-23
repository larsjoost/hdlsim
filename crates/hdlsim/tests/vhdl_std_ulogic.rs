//! Regression test for the reported error
//! `error[E0401]: unknown type 'std_ulogic'`: the ieee.std_logic_1164 type mark
//! `std_ulogic` must be known to the lowerer, with or without a `use` clause.

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
fn std_ulogic_ports_check_clean() {
    for f in ["std_ulogic_use.vhd", "std_ulogic_no_use.vhd"] {
        let out = hdlsim()
            .arg("check")
            .arg(fixture(f))
            .output()
            .expect("runs");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success(),
            "{}: expected exit 0, got {:?}\n{}{}",
            f,
            out.status,
            String::from_utf8_lossy(&out.stdout),
            stderr
        );
        assert!(!stderr.contains("E0401"), "{}: {}", f, stderr);
    }
}

#[test]
fn unknown_type_error_lists_known_marks() {
    let f = fixture("std_ulogic_use.vhd");
    let src = std::fs::read_to_string(&f)
        .unwrap()
        .replace("std_ulogic", "not_a_type");
    let tmp = std::env::temp_dir().join(format!("hdlsim_badtype_{}.vhd", std::process::id()));
    std::fs::write(&tmp, src).unwrap();
    let out = hdlsim().arg("check").arg(&tmp).output().expect("runs");
    let _ = std::fs::remove_file(&tmp);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unknown type 'not_a_type'"), "{}", stderr);
    assert!(
        stderr.contains("std_ulogic"),
        "help should list known marks: {}",
        stderr
    );
}
