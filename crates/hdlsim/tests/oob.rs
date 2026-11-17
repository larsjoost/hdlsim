//! Out-of-the-box unit resolution: running `hdlsim check top.vhd` inside a
//! project directory (no hdlsim.ini, no -L) must resolve `use work.pkg.all;`
//! from the project tree; a unit that exists nowhere must produce a warning
//! that says how to fix it (`hdlsim init` / `-L`); `hdlsim init` writes the
//! config.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.env("HDLSIM_NO_CACHE", "1");
    c
}

fn make_project(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("hdlsim_oob_{}_{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("rtl")).unwrap();
    std::fs::create_dir_all(dir.join("tb")).unwrap();
    std::fs::write(
        dir.join("rtl/my_pkg.vhd"),
        "package my_pkg is\n  constant WIDTH : natural := 8;\nend package;\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("tb/top.vhd"),
        "use work.my_pkg.all;\nentity top is end entity;\narchitecture rtl of top is\n  signal q : bit_vector(WIDTH-1 downto 0);\nbegin\n  q <= (others => '0');\nend architecture;\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("tb/bad.vhd"),
        "use work.no_such_pkg.all;\nentity bad is end entity;\narchitecture rtl of bad is begin end architecture;\n",
    )
    .unwrap();
    dir
}

#[test]
fn check_resolves_project_units_without_any_setup() {
    let dir = make_project("oob");
    let out = hdlsim()
        .args(["check", "tb/top.vhd"])
        .current_dir(&dir)
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
    assert!(stdout.contains("ok"), "{}{}", stdout, stderr);
    assert!(!stderr.contains("error["), "{}", stderr);
    assert!(!stderr.contains("W0501"), "{}", stderr);
}

#[test]
fn missing_unit_warning_points_to_init_and_lib_flag() {
    let dir = make_project("miss");
    let out = hdlsim()
        .args(["check", "tb/bad.vhd"])
        .current_dir(&dir)
        .output()
        .expect("runs");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no_such_pkg"), "{}", stderr);
    assert!(
        stderr.contains("hdlsim init") && stderr.contains("-L"),
        "warning must say how to fix it: {}",
        stderr
    );
}

#[test]
fn init_writes_hdlsim_ini_and_check_then_passes() {
    let dir = make_project("init");
    let out = hdlsim()
        .args(["init"])
        .current_dir(&dir)
        .output()
        .expect("runs");
    assert!(out.status.success(), "{:?}", out.status);
    let ini = std::fs::read_to_string(dir.join("hdlsim.ini")).expect("hdlsim.ini written");
    assert!(ini.contains("[Library]"), "{}", ini);
    assert!(ini.contains("work = ."), "{}", ini);
    // With the config present, check resolves with zero diagnostics.
    let out = hdlsim()
        .args(["check", "tb/top.vhd"])
        .current_dir(&dir)
        .output()
        .expect("runs");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{:?}\n{}", out.status, stderr);
    assert!(!stderr.contains("error["), "{}", stderr);
}

#[test]
fn search_fallback_warns_when_unit_found_outside_libraries() {
    // With an explicit -L mapping, `work = .` is not auto-added; the unit in
    // rtl/ is then found by the project search, with a W0502 hint.
    let dir = make_project("search");
    std::fs::create_dir_all(dir.join("elsewhere")).unwrap();
    std::fs::write(dir.join("elsewhere/x.v"), "// empty\n").unwrap();
    let out = hdlsim()
        .args(["check", "-L", "other=elsewhere", "tb/top.vhd"])
        .current_dir(&dir)
        .output()
        .expect("runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "got {:?}\n{}{}",
        out.status,
        stdout,
        stderr
    );
    assert!(stdout.contains("ok"), "{}{}", stdout, stderr);
    assert!(stderr.contains("W0502"), "{}", stderr);
    assert!(stderr.contains("hdlsim init"), "{}", stderr);
    assert!(
        stderr.contains("rtl/my_pkg.vhd") || stderr.contains("rtl\\my_pkg.vhd"),
        "{}",
        stderr
    );
}
