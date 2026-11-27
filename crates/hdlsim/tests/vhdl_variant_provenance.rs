//! Generic-map variant re-lowering must honor explicit-source provenance:
//! when a testbench instantiates a design unit with a `generic map`, the
//! variant module is re-lowered from the unit's source files — and those
//! must be the files the user actually passed on the command line, not a
//! same-named unit that happens to sit in a scanned library directory.
//!
//! Layout mirrors the trap: the working directory (`work = .`) holds the
//! testbench next to a same-named decoy entity/architecture pair (the
//! twin being replaced), while the real copies are passed explicitly from
//! a sibling directory. The generic map forces the variant re-lowering
//! path; a wrong-source bind drives the wrong value and the assertion
//! fails.

use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

/// One temp tree per test: `work/comp/` (cwd, holds the decoy + tb) and
/// `cli/` (the explicitly-passed copies). Returns (work_dir, cli_dir).
fn setup(tag: &str) -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir().join(format!("hdlsim_vprov_{}_{}", tag, std::process::id()));
    let work = base.join("work");
    let cli = base.join("cli");
    fs::create_dir_all(work.join("comp")).expect("create work/comp");
    fs::create_dir_all(&cli).expect("create cli");
    fs::write(work.join("hdlsim.ini"), "[Library]\nwork = .\n").expect("write hdlsim.ini");

    // Decoy: same entity/architecture names, drives the failing value; the
    // explicit copies drive the value the testbench demands.
    write_entity(&work.join("comp"));
    write_arch(&work.join("comp"), "  y <= '0';");
    write_entity(&cli);
    write_arch(&cli, "  y <= '1';");

    // The generic map forces the variant re-lowering path.
    write_tb(&work, "    generic map (G => 4)\n");
    (work, cli)
}

/// The entity declaration — identical for the decoy and the explicit copy.
fn write_entity(dir: &Path) {
    fs::write(
        dir.join("varprov_entity.vhd"),
        r#"library ieee;
use ieee.std_logic_1164.all;

entity varprov is
  generic (G : natural := 2);
  port (y : out std_logic);
end entity;
"#,
    )
    .expect("write entity");
}

/// The architecture body — the only behavioral difference between the
/// same-named copies.
fn write_arch(dir: &Path, body: &str) {
    fs::write(
        dir.join("varprov_rtl.vhd"),
        format!(
            r#"library ieee;
use ieee.std_logic_1164.all;

architecture rtl of varprov is
begin
{body}
end architecture;
"#
        ),
    )
    .expect("write arch");
}

/// The testbench, parameterized over the DUT instantiation's generic map
/// (empty for the plain-path control test).
fn write_tb(work: &Path, generic_map: &str) {
    fs::write(
        work.join("comp").join("varprov_tb.vhd"),
        format!(
            r#"library ieee;
use ieee.std_logic_1164.all;

entity varprov_tb is
end entity;

architecture sim of varprov_tb is
  signal y : std_logic;
begin
  dut : entity work.varprov(rtl)
{generic_map}    port map (y => y);
  main : process
  begin
    wait for 1 ns;
    assert y = '1'
      report "variant re-lowered from the wrong source file"
      severity failure;
    wait;
  end process;
end architecture;
"#
        ),
    )
    .expect("write tb");
}

fn run(work: &Path, cli: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_hdlsim"))
        .current_dir(work)
        .arg("--no-cache")
        .args(["run", "--top", "varprov_tb"])
        .arg(cli.join("varprov_entity.vhd"))
        .arg(cli.join("varprov_rtl.vhd"))
        .arg("comp/varprov_tb.vhd")
        .args(args)
        .output()
        .expect("hdlsim runs");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The explicitly-passed copies must win the variant bind: with the generic
/// map the variant module is re-lowered from the CLI sources, so `y` is '1'
/// even though a same-named decoy sits next to the testbench.
#[test]
fn generic_variant_relowers_from_the_explicit_sources() {
    let (work, cli) = setup("generic");
    let (ok, err) = run(&work, &cli, &["--max-time", "100ns"]);
    assert!(ok, "explicit sources must win the variant bind: {err}");
    assert!(!err.contains("wrong source file"), "decoy was bound: {err}");
}

/// Control for the plain path (no generic map): the explicit copies bind
/// directly and must keep doing so.
#[test]
fn plain_instantiation_binds_the_explicit_sources() {
    let (work, cli) = setup("plain");
    // Drop the generic map from the testbench.
    write_tb(&work, "");
    let (ok, err) = run(&work, &cli, &["--max-time", "100ns"]);
    assert!(ok, "explicit sources must win the plain bind: {err}");
    assert!(!err.contains("wrong source file"), "decoy was bound: {err}");
}
