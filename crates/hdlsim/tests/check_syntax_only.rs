//! `hdlsim check --syntax-only`: parse one file standalone — no dependency
//! compilation, no library resolution. A file that references units it does
//! not define still passes when its syntax is valid; a malformed file fails
//! with exit 1.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.env("HDLSIM_NO_CACHE", "1");
    c
}

fn write_tmp(file_name: &str, text: &str) -> PathBuf {
    // Keep the extension: the frontend is picked by it.
    let stem = PathBuf::from(file_name)
        .with_extension("")
        .into_os_string()
        .into_string()
        .unwrap();
    let ext = PathBuf::from(file_name)
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let p = std::env::temp_dir().join(format!("{stem}_{}{ext}", std::process::id()));
    std::fs::write(&p, text).unwrap();
    p
}

#[test]
fn syntax_only_passes_without_resolving_dependencies() {
    // `work.missing_pkg` does not exist anywhere — full `check` would try to
    // resolve it; syntax-only must not.
    let f = write_tmp(
        "syntax_only_ok.vhd",
        "LIBRARY ieee;\n\
         USE ieee.std_logic_1164.all;\n\
         USE work.missing_pkg.all;\n\
         ENTITY syntax_only_ok IS\n\
         END ENTITY syntax_only_ok;\n",
    );
    let out = hdlsim()
        .args(["check", "--syntax-only"])
        .arg(&f)
        .output()
        .expect("runs");
    let _ = std::fs::remove_file(&f);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}");
    assert!(stdout.contains("ok (1 file(s))"), "{stdout}");
}

#[test]
fn syntax_only_reports_syntax_errors_with_exit_1() {
    let f = write_tmp(
        "syntax_only_bad.vhd",
        "ENTITY syntax_only_bad IS\n\
         3 SIGNAL x : bit;\n\
         END ENTITY syntax_only_bad;\n",
    );
    let out = hdlsim()
        .args(["check", "--syntax-only"])
        .arg(&f)
        .output()
        .expect("runs");
    let _ = std::fs::remove_file(&f);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("error["), "{stderr}");
    assert!(stderr.contains("syntax_only_bad"), "{stderr}");
}

#[test]
fn syntax_only_parses_verilog_and_vhdl_by_extension() {
    let v = write_tmp(
        "syntax_only_v.v",
        "module syntax_only_v;\n  initial $display(\"hi\");\nendmodule\n",
    );
    let out = hdlsim()
        .args(["check", "--syntax-only"])
        .arg(&v)
        .output()
        .expect("runs");
    let _ = std::fs::remove_file(&v);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}");
}
