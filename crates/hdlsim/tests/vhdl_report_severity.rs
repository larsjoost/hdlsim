//! VHDL report/assert severity in the transcript: every severity-carrying
//! message is prefixed with the emitting statement's `file:line:` location,
//! tagged with its severity level (`[note]` … `[failure]`), color-coded
//! (warning yellow, error/failure red, note plain), and suffixed with the
//! emitting instance's hierarchy path `(at inst.sub)` when the kernel knows
//! it. A severity written as a named `severity_level` constant takes
//! effect — it must not be silently dropped to note.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

/// Write `<name>.vhd` (its basename is the `file:` part of transcript
/// locations, so tests assert on `<name>.vhd:<line>:`).
fn write_src(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("hdlsim_sev_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(format!("{name}.vhd"));
    std::fs::write(&p, src).unwrap();
    p
}

/// A one-report testbench whose REPORT sits on line 6.
fn sev_src(name: &str, stmt: &str) -> PathBuf {
    let src = format!(
        "entity sev_tb is end entity;\narchitecture a of sev_tb is\nbegin\n  process is\n  begin\n    {stmt}\n    wait;\n  end process;\nend architecture;\n"
    );
    assert!(
        src.lines().nth(5).unwrap().contains("report"),
        "test bug: statement is not on line 6:\n{src}"
    );
    write_src(name, &src)
}

fn run(top: &str, args: &[&str], file: &PathBuf) -> (String, Option<i32>) {
    let out = hdlsim()
        .args([
            "run",
            "--top",
            top,
            "--max-time",
            "100ns",
            "--max-wall",
            "30s",
        ])
        .args(args)
        .arg(file)
        .output()
        .expect("runs hdlsim");
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    (text, out.status.code())
}

#[test]
fn note_report_is_located_and_plain() {
    let f = sev_src("sev_note", "report \"hello\";");
    let (out, code) = run("sev_tb", &["--color", "always"], &f);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("sev_note.vhd:6: [note] hello"), "{out}");
    // Note severity adds no color even with --color always, and a passing
    // run paints nothing else.
    assert!(!out.contains('\x1b'), "{out}");
    let _ = std::fs::remove_file(&f);
}

#[test]
fn warning_report_is_located_and_yellow() {
    let f = sev_src("sev_warn", "report \"careful\" severity warning;");
    let (out, code) = run("sev_tb", &["--color", "always"], &f);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("sev_warn.vhd:6: [warning] careful"), "{out}");
    assert!(out.contains("\x1b[1;33m"), "{out}");
    // Plain mode keeps the location, drops the escape.
    let (out, code) = run("sev_tb", &["--no-color"], &f);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("sev_warn.vhd:6: [warning] careful"), "{out}");
    assert!(!out.contains('\x1b'), "{out}");
    let _ = std::fs::remove_file(&f);
}

#[test]
fn error_report_fails_run_in_red() {
    let f = sev_src("sev_err", "report \"boom\" severity error;");
    let (out, code) = run("sev_tb", &["--color", "always"], &f);
    assert_eq!(code, Some(1), "{out}");
    assert!(out.contains("sev_err.vhd:6: [error] boom"), "{out}");
    assert!(out.contains("\x1b[1;31m"), "{out}");
    let _ = std::fs::remove_file(&f);
}

#[test]
fn failure_severity_aborts_simulation() {
    let f = sev_src("sev_fail", "report \"dead\" severity failure;");
    let (out, code) = run("sev_tb", &["--no-color"], &f);
    assert_eq!(code, Some(1), "{out}");
    assert!(out.contains("sev_fail.vhd:6: [failure] dead"), "{out}");
    assert!(out.contains("severity failure"), "{out}");
    let _ = std::fs::remove_file(&f);
}

#[test]
fn named_constant_severity_takes_effect() {
    // A severity written as a severity_level constant (`severity C_SEV`) is
    // const-folded by the lowerer: error fails the run like the keyword does.
    let src = "entity sev_const_tb is end entity;\narchitecture a of sev_const_tb is\n  constant C_SEV : severity_level := error;\nbegin\n  process is\n  begin\n    report \"dyn boom\" severity C_SEV;\n    wait;\n  end process;\nend architecture;\n";
    let f = write_src("sev_const", src);
    let (out, code) = run("sev_const_tb", &["--no-color"], &f);
    assert_eq!(code, Some(1), "{out}");
    assert!(out.contains("sev_const.vhd:7: [error] dyn boom"), "{out}");
    let _ = std::fs::remove_file(&f);
}

#[test]
fn assert_severity_follows_the_same_rules() {
    // Warning assertion: reported, located, run still passes.
    let f = sev_src(
        "sev_awarn",
        "assert false report \"soft\" severity warning;",
    );
    let (out, code) = run("sev_tb", &["--color", "always"], &f);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("sev_awarn.vhd:6: [warning] soft"), "{out}");
    assert!(out.contains("\x1b[1;33m"), "{out}");
    let _ = std::fs::remove_file(&f);

    // Default assert severity is error: fails the run, in red.
    let f = sev_src("sev_aerr", "assert 1 = 2 report \"hard\";");
    let (out, code) = run("sev_tb", &["--color", "always"], &f);
    assert_eq!(code, Some(1), "{out}");
    assert!(out.contains("sev_aerr.vhd:6: "), "{out}");
    assert!(out.contains("assertion failed"), "{out}");
    assert!(out.contains("\x1b[1;31m"), "{out}");
    let _ = std::fs::remove_file(&f);
}

#[test]
fn report_lines_carry_the_emitting_scope_path() {
    // A report from a nested instance is suffixed with the instance path —
    // the same dotted names `--list-signals` prints — so an agent can tell
    // WHICH instance fired when a component is instantiated more than once.
    // A top-level report has no path suffix. (Leaf report: line 8, top
    // report: line 34.)
    const SRC: &str = "\
entity scope_leaf is
  port (done : out bit);
end entity;
architecture rtl of scope_leaf is
begin
  process is
  begin
    report \"leaf report\" severity warning;
    done <= '1';
    wait;
  end process;
end architecture;

entity scope_mid is
  port (done : out bit);
end entity;
architecture rtl of scope_mid is
  signal d : bit;
begin
  u_leaf : entity work.scope_leaf port map (done => d);
  done <= d;
end architecture;

entity scope_tb is
  port (ok : out bit);
end entity;
architecture a of scope_tb is
  signal m : bit;
begin
  u_mid : entity work.scope_mid port map (done => m);
  ok <= m;
  process is
  begin
    report \"top report\";
    wait;
  end process;
end architecture;
";
    let f = write_src("sev_scope", SRC);
    let (out, code) = run("scope_tb", &["--no-color"], &f);
    assert_eq!(code, Some(0), "{out}");
    assert!(
        out.contains("sev_scope.vhd:8: [warning] leaf report (at u_mid.u_leaf)"),
        "{out}"
    );
    assert!(
        out.contains("sev_scope.vhd:34: [note] top report\n"),
        "{out}"
    );
    let _ = std::fs::remove_file(&f);
}
