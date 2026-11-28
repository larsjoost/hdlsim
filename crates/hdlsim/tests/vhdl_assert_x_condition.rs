//! An X-valued assertion condition is *unknown*, not false: the assertion
//! must not fire — the same X semantics the `if` statement applies (unknown
//! condition → no action). A definitely-false condition still fires,
//! including the `assert false` end-of-test marker and ordinary failing
//! assertions.
//!
//! Layout: `axchk` derives an X `natural` from an X vector and guards
//! assertions with it; separate runs cover the must-fire controls.

use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

/// One temp tree per test: cwd holds the source, passed explicitly.
fn setup(tag: &str) -> PathBuf {
    let work = std::env::temp_dir().join(format!("hdlsim_assertx_{}_{}", tag, std::process::id()));
    fs::create_dir_all(&work).expect("create work dir");
    work
}

/// Entity + process body: an X `natural` from `to_integer(unsigned(xv))`,
/// then `body` (the assertions under test).
fn write_src(work: &Path, tag: &str, body: &str) {
    let src = format!(
        r#"library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;

entity axchk is
end entity;

architecture sim of axchk is
begin
  main : process
    variable xv : std_ulogic_vector(7 downto 0) := (others => 'x');
    variable xi : natural;
  begin
    wait for 1 ns;
    xi := to_integer(unsigned(xv));
{body}
    wait;
  end process;
end architecture;
"#,
        body = body
    );
    fs::write(work.join(format!("{tag}.vhd")), src).expect("write source");
}

/// Run `axchk`; returns (exit ok, combined output).
fn run(work: &Path, tag: &str) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_hdlsim"))
        .current_dir(work)
        .arg("--no-cache")
        .args(["run", &format!("{tag}.vhd"), "--top", "axchk"])
        .args(["--max-time", "100ns", "--no-color"])
        .output()
        .expect("hdlsim runs");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// All four comparison forms on an X value are unknown: none may fire.
#[test]
fn x_condition_assertion_does_not_fire() {
    let work = setup("xcond");
    write_src(
        &work,
        "xcond",
        r#"    ASSERT (xi <= 0) REPORT "BUG: x <= 0 fired" SEVERITY failure;
    ASSERT (xi > 0) REPORT "BUG: x > 0 fired" SEVERITY failure;
    ASSERT (xi = 0) REPORT "BUG: x = 0 fired" SEVERITY failure;
    ASSERT (xi /= 0) REPORT "BUG: x /= 0 fired" SEVERITY failure;
"#,
    );
    let (ok, out) = run(&work, "xcond");
    assert!(ok, "X-condition assertions must not fail the run: {out}");
    assert!(!out.contains("BUG:"), "no X assertion may fire: {out}");
}

/// A definitely-false condition still fires the assertion (and fails the run
/// at severity failure).
#[test]
fn definitely_false_condition_still_fires() {
    let work = setup("dffalse");
    write_src(
        &work,
        "dffalse",
        r#"    ASSERT (1 = 2) REPORT "control: definite false" SEVERITY failure;
"#,
    );
    let (ok, out) = run(&work, "dffalse");
    assert!(!ok, "a definitely-false assertion must fail the run: {out}");
    assert!(
        out.contains("control: definite false"),
        "the report message must appear: {out}"
    );
}

/// The `assert false` end-of-test marker keeps firing (as a note it prints
/// without failing the run).
#[test]
fn assert_false_marker_still_fires() {
    let work = setup("marker");
    write_src(
        &work,
        "marker",
        r#"    ASSERT false REPORT "end-of-test marker" SEVERITY note;
"#,
    );
    let (ok, out) = run(&work, "marker");
    assert!(ok, "a note-severity marker must not fail the run: {out}");
    assert!(
        out.contains("end-of-test marker"),
        "the marker message must appear: {out}"
    );
}
