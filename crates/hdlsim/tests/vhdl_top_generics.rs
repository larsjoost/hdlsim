//! `-g NAME=VALUE` top-generic overrides must reach the architecture: the
//! top is re-lowered with the values applied (like a generic-map variant),
//! so signal widths, ranges, and generate decisions computed from the
//! generics see them — not just the parameter map.
//!
//! Layout: `gtop` declares unbound generics (`W : positive`, `EN : boolean`
//! — no defaults, so without overrides they are X) and instantiates
//! `gchild`, whose 64-bit output lands on a `0 TO W - 1` signal. The tb
//! asserts the overridden values and that the child's driven bit is visible
//! through the now-correctly-sized connect.

use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

/// One temp tree per test: cwd holds `hdlsim.ini` and the sources, all
/// passed explicitly on the command line.
fn setup(tag: &str) -> PathBuf {
    let work = std::env::temp_dir().join(format!("hdlsim_topgen_{}_{}", tag, std::process::id()));
    fs::create_dir_all(&work).expect("create work dir");
    fs::write(work.join("hdlsim.ini"), "[Library]\nwork = .\n").expect("write hdlsim.ini");
    write_child(&work);
    write_top(&work);
    work
}

/// `gchild`: a 64-bit (default-generic) output with bit 0 driven '1' in
/// both 32-bit halves, so the assertion is independent of the connect's
/// word packing order.
fn write_child(work: &Path) {
    fs::write(
        work.join("gchild_entity.vhd"),
        r#"library ieee;
use ieee.std_logic_1164.all;

entity gchild is
  generic (G_W : positive := 64);
  port (o : out std_ulogic_vector(0 TO G_W - 1));
end entity;
"#,
    )
    .expect("write child entity");
    fs::write(
        work.join("gchild_rtl.vhd"),
        r#"library ieee;
use ieee.std_logic_1164.all;

architecture rtl of gchild is
begin
  o(0) <= '1';
  o(32) <= '1';
  o(1 TO 31) <= (OTHERS => '0');
  o(33 TO 63) <= (OTHERS => '0');
end architecture;
"#,
    )
    .expect("write child arch");
}

/// `gtop`: unbound `W`/`EN` generics; the process asserts the override
/// landed in the architecture's declarations and data path.
fn write_top(work: &Path) {
    fs::write(
        work.join("gtop_entity.vhd"),
        r#"library ieee;
use ieee.std_logic_1164.all;

entity gtop is
  generic (W : positive; EN : boolean);
  port (ok : out std_ulogic);
end entity;
"#,
    )
    .expect("write top entity");
    fs::write(
        work.join("gtop_sim.vhd"),
        r#"library ieee;
use ieee.std_logic_1164.all;

architecture sim of gtop is
  signal s : std_ulogic_vector(0 TO W - 1);
begin
  c : entity work.gchild port map (o => s);
  ok <= '1' when EN and s(0) = '1' else '0';
  main : process
  begin
    wait for 1 ns;
    assert W = 64
      report "generic W did not reach the architecture" severity failure;
    assert EN
      report "generic EN did not reach the architecture" severity failure;
    assert s(0) = '1'
      report "child bit not visible through the sized connect" severity failure;
    wait;
  end process;
end architecture;
"#,
    )
    .expect("write top arch");
}

/// Run `gtop` with the given `-g` specs; returns (exit ok, combined output).
fn run(work: &PathBuf, generics: &[&str]) -> (bool, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    cmd.current_dir(work)
        .arg("--no-cache")
        .args(["run", "--top", "gtop"])
        .arg("gtop_entity.vhd")
        .arg("gtop_sim.vhd")
        .arg("gchild_entity.vhd")
        .arg("gchild_rtl.vhd")
        .args(["--max-time", "100ns", "--no-color"]);
    for g in generics {
        cmd.args(["-g", g]);
    }
    let out = cmd.output().expect("hdlsim runs");
    (
        out.status.success(),
        // hdlsim prints its transcript and diagnostics on stdout by default
        // (only --error-format json goes to stderr).
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// Decimal integer + boolean overrides drive the architecture: the tb's
/// assertions on `W`, `EN`, and the connected data all pass.
#[test]
fn top_generic_overrides_reach_the_architecture() {
    let work = setup("decimal");
    let (ok, out) = run(&work, &["W=64", "EN=true"]);
    assert!(ok, "run with overrides should pass: {out}");
}

/// VHDL based literals are accepted (`16#40#` = 64).
#[test]
fn top_generic_overrides_accept_based_literals() {
    let work = setup("based");
    let (ok, out) = run(&work, &["W=16#40#", "EN=true"]);
    assert!(ok, "run with based-literal override should pass: {out}");
}

/// An override naming no declared generic is a hard error — a typo'd knob
/// must not fail silently.
#[test]
fn top_generic_override_unknown_name_is_a_hard_error() {
    let work = setup("unknown");
    let (ok, out) = run(&work, &["NOPE=1"]);
    assert!(!ok, "unknown generic must fail the run: {out}");
    assert!(
        out.contains("unknown generic 'NOPE'"),
        "error should name the generic: {out}"
    );
}

/// An unparseable value is a hard error naming the generic and the value.
#[test]
fn top_generic_override_unparseable_value_is_a_hard_error() {
    let work = setup("badvalue");
    let (ok, out) = run(&work, &["W=abc"]);
    assert!(!ok, "unparseable value must fail the run: {out}");
    assert!(
        out.contains("cannot parse value 'abc' for generic 'W'"),
        "error should name the generic and value: {out}"
    );
}
