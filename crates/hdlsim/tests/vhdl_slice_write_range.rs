//! An out-of-range slice write must be a no-op, not a clamp onto bit 0.
//!
//! Reproduces the false "combinational loop": a top entity whose width
//! generic is unbound (no default) lowers a signal to width 1, while the
//! connected child port keeps its own default width (64 = two memory
//! words), so the connect lowering emits two slice writes beyond the
//! signal's width. The kernel used to clamp both onto bit 0; with two
//! different sources the bit toggled every fixpoint sweep and the run
//! aborted with "combinational loop detected" even though the static
//! assign graph is acyclic.
//!
//! `wide_child` drives the two halves of its 64-bit output with different
//! bit-0 values so the clamped writes disagree.

use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

/// One temp tree per test: cwd holds `hdlsim.ini` and the sources; every
/// file is passed explicitly, so no library scan can interfere.
fn setup(tag: &str, top_default: Option<u64>) -> PathBuf {
    let work =
        std::env::temp_dir().join(format!("hdlsim_combclamp_{}_{}", tag, std::process::id()));
    fs::create_dir_all(&work).expect("create work dir");
    fs::write(work.join("hdlsim.ini"), "[Library]\nwork = .\n").expect("write hdlsim.ini");
    write_child(&work);
    write_top(&work, top_default);
    work
}

/// `wide_child`: a 64-bit (default-generic) output whose low word's bit 0
/// is '1' and high word's bit 0 is '0' — the two half-writes disagree.
fn write_child(work: &Path) {
    fs::write(
        work.join("wide_child_entity.vhd"),
        r#"library ieee;
use ieee.std_logic_1164.all;

entity wide_child is
  generic (G_W : positive := 64);
  port (o : out std_ulogic_vector(0 TO G_W - 1));
end entity;
"#,
    )
    .expect("write child entity");
    fs::write(
        work.join("wide_child_rtl.vhd"),
        r#"library ieee;
use ieee.std_logic_1164.all;

architecture rtl of wide_child is
begin
  o(0) <= '1';
  o(32) <= '0';
  o(1 TO 31) <= (OTHERS => '0');
  o(33 TO 63) <= (OTHERS => '0');
end architecture;
"#,
    )
    .expect("write child arch");
}

/// `clamp_top`: an unbound `W` generic (no default, unless the test passes
/// one) leaves `s` at width 1 while the child port stays 64 bits wide.
fn write_top(work: &Path, top_default: Option<u64>) {
    let generic_line = match top_default {
        Some(w) => format!("  generic (W : positive := {w});\n"),
        None => "  generic (W : positive);\n".to_string(),
    };
    fs::write(
        work.join("clamp_top_entity.vhd"),
        format!(
            r#"library ieee;
use ieee.std_logic_1164.all;

entity clamp_top is
{generic_line}  port (ok : out std_ulogic);
end entity;
"#
        ),
    )
    .expect("write top entity");
    fs::write(
        work.join("clamp_top_rtl.vhd"),
        r#"library ieee;
use ieee.std_logic_1164.all;

architecture sim of clamp_top is
  signal s : std_ulogic_vector(0 TO W - 1);
begin
  c : entity work.wide_child port map (o => s);
  ok <= s(0);
end architecture;
"#,
    )
    .expect("write top arch");
}

/// Run `clamp_top` from the work dir with all sources explicit.
fn run(work: &PathBuf) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_hdlsim"))
        .current_dir(work)
        .args(["--no-cache", "run", "--top", "clamp_top"])
        .arg("clamp_top_entity.vhd")
        .arg("clamp_top_rtl.vhd")
        .arg("wide_child_entity.vhd")
        .arg("wide_child_rtl.vhd")
        .args(["--max-time", "100ns", "--no-color"])
        .output()
        .expect("hdlsim runs");
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

/// A top with an unbound width generic (signal width 1) connected to a
/// 64-bit child port must not trip the combinational-loop guard: the
/// out-of-range slice writes are no-ops, the fixpoint converges, and the
/// run completes.
#[test]
fn unbound_generic_width_mismatch_does_not_trip_comb_loop() {
    let work = setup("unbound", None);
    let (ok, err) = run(&work);
    assert!(
        !err.contains("combinational loop"),
        "false combinational loop on out-of-range slice writes: {err}"
    );
    assert!(ok, "run should complete: {err}");
}

/// Control: with the generic bound (signal width matches the child port)
/// the connect is in-range and the run is clean.
#[test]
fn bound_generic_connect_runs_clean() {
    let work = setup("bound", Some(64));
    let (ok, err) = run(&work);
    assert!(ok, "in-range connect should run clean: {err}");
    assert!(!err.contains("combinational loop"), "{err}");
}
