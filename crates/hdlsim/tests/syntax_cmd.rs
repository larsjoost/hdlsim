//! `hdlsim syntax` — the short parse-only check (same engine as
//! `check --syntax-only`): valid files pass without dependency resolution,
//! malformed files fail with exit 1.

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
fn syntax_passes_without_resolving_dependencies() {
    // `work.missing_pkg` does not exist anywhere — syntax must not care.
    let f = write_tmp(
        "syntax_cmd_ok.vhd",
        "LIBRARY ieee;\n\
         USE ieee.std_logic_1164.all;\n\
         USE work.missing_pkg.all;\n\
         ENTITY syntax_cmd_ok IS\n\
         END ENTITY syntax_cmd_ok;\n",
    );
    let out = hdlsim().arg("syntax").arg(&f).output().expect("runs");
    let _ = std::fs::remove_file(&f);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}");
    assert!(stdout.contains("hdlsim syntax: ok (1 file(s))"), "{stdout}");
}

#[test]
fn syntax_reports_syntax_errors_with_exit_1() {
    let f = write_tmp(
        "syntax_cmd_bad.vhd",
        "ENTITY syntax_cmd_bad IS\n\
         3 SIGNAL x : bit;\n\
         END ENTITY syntax_cmd_bad;\n",
    );
    let out = hdlsim().arg("syntax").arg(&f).output().expect("runs");
    let _ = std::fs::remove_file(&f);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("error["), "{stderr}");
    assert!(stderr.contains("syntax_cmd_bad"), "{stderr}");
}

#[test]
fn syntax_is_listed_in_help() {
    let out = hdlsim().arg("--help").output().expect("runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("syntax"), "{stdout}");
}
