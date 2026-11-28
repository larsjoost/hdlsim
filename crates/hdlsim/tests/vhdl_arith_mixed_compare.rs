//! std_logic_arith mixed comparisons (`unsigned`/`signed` vs `integer`)
//! convert the integer operand to a vector before comparing, so the
//! integer wraps instead of the vector extending. Vendor semantics
//! (pinned by dense scans of the integer operand):
//!
//! * ordered (`<` family): the integer wraps mod 2ⁿ — `conv_unsigned` for
//!   an `unsigned` vector, `conv_signed` for a `signed` one — so
//!   `unsigned("1111") < 16` is false (16 wraps to 0) and
//!   `signed("1111") < 8` is false (8 wraps to -8).
//! * equality with a `signed` vector: the integer wraps mod 2ⁿ signed —
//!   `signed("1111") = 15` is true (15 wraps to -1).
//! * equality with an `unsigned` vector: the integer wraps mod 2ⁿ⁺¹ as a
//!   signed value against the zero-padded vector — `unsigned("1111") = 47`
//!   is true, `= 31` is false.
//!
//! Same-type vector comparisons of different lengths keep the
//! extend-to-wider behavior.
//!
//! Layout: one design per operand shape; each assertion demands the
//! vendor-matching outcome, so a clean run proves every comparison.

use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

/// One temp tree per test: cwd holds the source, passed explicitly.
fn setup(tag: &str) -> PathBuf {
    let work = std::env::temp_dir().join(format!("hdlsim_arithcmp_{}_{}", tag, std::process::id()));
    fs::create_dir_all(&work).expect("create work dir");
    work
}

/// Entity whose sequential process evaluates `body` after 1 ns.
fn write_src(work: &Path, tag: &str, decls: &str, body: &str) {
    let src = format!(
        r#"library ieee;
use ieee.std_logic_1164.all;
use ieee.std_logic_arith.all;

entity acchk is
end entity;

architecture sim of acchk is
{decls}
begin
  main : process
  begin
    wait for 1 ns;
{body}
    wait;
  end process;
end architecture;
"#,
        decls = decls,
        body = body,
    );
    fs::write(work.join(format!("{tag}.vhd")), src).expect("write source");
}

/// Run `acchk`; returns (exit ok, combined output).
fn run(work: &Path, tag: &str) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_hdlsim"))
        .current_dir(work)
        .arg("--no-cache")
        .args(["run", &format!("{tag}.vhd"), "--top", "acchk"])
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

/// `signed` vector compared with an out-of-range positive integer: the
/// integer wraps mod 2ⁿ signed (`15` → -1 in 4 bits). This is the idiom
/// segment counters use to detect their last value
/// (`signed(counter) = G_SIZE - 1` with a log2-wide counter).
#[test]
fn signed_vector_eq_wrapped_integer() {
    let work = setup("seq");
    write_src(
        &work,
        "seq",
        "  signal cnt : unsigned(0 to 3) := \"1111\";\n",
        r#"    ASSERT (signed(cnt) = 15) REPORT "BUG: signed(1111) = 15 must be true" SEVERITY failure;
    ASSERT (signed(cnt) = -1) REPORT "BUG: signed(1111) = -1 must be true" SEVERITY failure;
"#,
    );
    let (ok, out) = run(&work, "seq");
    assert!(ok, "wrapped signed comparison must hold: {out}");
}

/// `unsigned` mixed comparisons: ordered operands wrap mod 2ⁿ unsigned
/// (`16` → 0, so `> 16` is true and `< 16`/`< 17` are false); equality
/// wraps mod 2ⁿ⁺¹ signed (`31` → -1 ≠ 15, `47` → 15 = 15).
#[test]
fn unsigned_vector_mixed_comparisons_wrap() {
    let work = setup("uwrap");
    write_src(
        &work,
        "uwrap",
        "  signal vec : unsigned(0 to 3) := \"1111\";\n",
        r#"    ASSERT not (unsigned(vec) = 31) REPORT "BUG: unsigned(1111) = 31 must be false (wraps as signed in 5 bits)" SEVERITY failure;
    ASSERT unsigned(vec) > 16 REPORT "BUG: unsigned(1111) > 16 must be true (16 wraps to 0)" SEVERITY failure;
    ASSERT not (unsigned(vec) < 16) REPORT "BUG: unsigned(1111) < 16 must be false (16 wraps to 0)" SEVERITY failure;
    ASSERT not (unsigned(vec) < 17) REPORT "BUG: unsigned(1111) < 17 must be false (17 wraps to 1)" SEVERITY failure;
    ASSERT unsigned(vec) = 47 REPORT "BUG: unsigned(1111) = 47 must be true (47 wraps to 15)" SEVERITY failure;
"#,
    );
    let (ok, out) = run(&work, "uwrap");
    assert!(ok, "wrapped unsigned comparisons must hold: {out}");
}

/// In-range integer operands keep their mathematical meaning on both
/// operand orders, and same-type comparisons of different lengths extend
/// rather than wrap.
#[test]
fn in_range_and_same_type_comparisons_unchanged() {
    let work = setup("inrange");
    write_src(
        &work,
        "inrange",
        "  signal p : unsigned(0 to 3) := \"0111\";\n  signal w : unsigned(0 to 7) := \"00010000\";\n",
        r#"    ASSERT unsigned(p) = 7 REPORT "BUG: unsigned(0111) = 7 must be true" SEVERITY failure;
    ASSERT not (unsigned(p) = 8) REPORT "BUG: unsigned(0111) = 8 must be false" SEVERITY failure;
    ASSERT 7 = unsigned(p) REPORT "BUG: 7 = unsigned(0111) must be true" SEVERITY failure;
    ASSERT signed(p) = 7 REPORT "BUG: signed(0111) = 7 must be true" SEVERITY failure;
    ASSERT not (unsigned(w) = p) REPORT "BUG: uns8(16) = uns4(7) must be false (extend, not wrap)" SEVERITY failure;
"#,
    );
    let (ok, out) = run(&work, "inrange");
    assert!(ok, "in-range and same-type comparisons must hold: {out}");
}

/// Bare declared `unsigned` objects participate in the wrap (no explicit
/// conversion call) — the counter-plus-constant idiom.
#[test]
fn declared_unsigned_object_wraps_integer() {
    let work = setup("decl");
    write_src(
        &work,
        "decl",
        "  signal cnt : unsigned(3 downto 0) := \"1111\";\n",
        r#"    ASSERT cnt = 15 REPORT "BUG: cnt = 15 must be true" SEVERITY failure;
    ASSERT cnt /= 31 REPORT "BUG: cnt /= 31 must be true (equality wraps as signed in 5 bits)" SEVERITY failure;
    ASSERT not (cnt < 16) REPORT "BUG: cnt < 16 must be false (16 wraps to 0)" SEVERITY failure;
"#,
    );
    let (ok, out) = run(&work, "decl");
    assert!(ok, "declared-object mixed comparisons must hold: {out}");
}
