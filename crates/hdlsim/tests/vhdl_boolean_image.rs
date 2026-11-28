//! `boolean'image` must spell `true`/`false` for runtime (non-folded)
//! operands too. The decimal fallback of the string conversion reads as
//! `-1`/`1`/`0` depending on the comparison encoding — actively misleading
//! in a testbench transcript. X/unknown stays visible as `x`, and
//! `integer'image` keeps its decimal rendering.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const TB: &str = r#"library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;

entity boolimg_tb is
end entity;

architecture sim of boolimg_tb is
  signal clk  : std_logic := '0';
  signal cnt  : natural := 0;
  signal unk  : std_logic;  -- 'U'
  signal sig4 : signed(3 downto 0) := "0111";  -- 7
begin
  clk <= not clk after 5 ns;
  process (clk)
  begin
    if rising_edge(clk) then
      cnt <= cnt + 1;
    end if;
  end process;
  process
  begin
    report "FOLDED=" & boolean'image(1 = 1);
    report "LT=" & boolean'image(sig4 < 8);
    report "EQ=" & boolean'image(sig4 = 7);
    report "NE=" & boolean'image(sig4 = 9);
    report "UNK=" & boolean'image(unk = '1');
    report "INT=" & integer'image(-1);
    wait;
  end process;
end architecture;
"#;

/// A second tb whose boolean'image reports fire at 10/20/30 ns, so a
/// checkpoint at 15 ns splits the transcript: the resumed run must
/// re-evaluate the deserialized `ToString` expressions.
const TB_PHASED: &str = r#"library ieee;
use ieee.std_logic_1164.all;

entity boolimg_tb is
end entity;

architecture sim of boolimg_tb is
  signal clk : std_logic := '0';
  signal cnt : natural := 0;
begin
  clk <= not clk after 5 ns;
  process (clk)
  begin
    if rising_edge(clk) then
      cnt <= cnt + 1;
    end if;
  end process;
  process
  begin
    wait for 10 ns;
    report "LT=" & boolean'image(cnt < 3);
    wait for 10 ns;
    report "EQ=" & boolean'image(cnt = 2);
    wait for 10 ns;
    report "NE=" & boolean'image(cnt /= 3);
    wait;
  end process;
end architecture;
"#;

fn setup(tag: &str, tb: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("hdlsim_bimg_{}_{}", tag, std::process::id()));
    fs::create_dir_all(&dir).expect("create tmp dir");
    fs::write(dir.join("tb.vhd"), tb).expect("write tb");
    dir
}

fn run(dir: &Path, extra: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_hdlsim"))
        .current_dir(dir)
        .arg("--no-cache")
        .args(["run", "--top", "boolimg_tb", "tb.vhd", "--no-color"])
        .args(extra)
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

#[test]
fn boolean_image_renders_true_false_at_runtime() {
    let dir = setup("render", TB);
    let (ok, out) = run(&dir, &["--max-time", "1us"]);
    assert!(ok, "run must succeed: {}", out);
    // Runtime comparisons under a boolean prefix spell true/false — never
    // the decimal fallback (-1/1/0) of the underlying encoding.
    assert!(out.contains("FOLDED=true"), "{}", out);
    assert!(out.contains("LT=true"), "{}", out);
    assert!(out.contains("EQ=true"), "{}", out);
    assert!(out.contains("NE=false"), "{}", out);
    // Unknown stays visible as x.
    assert!(out.contains("UNK=x"), "{}", out);
    // Control: integer'image keeps its decimal rendering.
    assert!(out.contains("INT=-1"), "{}", out);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn boolean_image_survives_checkpoint_restore() {
    let dir = setup("ckpt", TB_PHASED);
    let ck = dir.join("ck.bin");
    let _ = fs::remove_file(&ck);

    // First leg: run to 15 ns and checkpoint. The 10 ns report must already
    // render, and the checkpoint must serialize the ToString expressions.
    let (ok, out) = run(
        &dir,
        &["--max-time", "15ns", "--checkpoint", ck.to_str().unwrap()],
    );
    assert!(ok, "checkpoint leg must succeed: {}", out);
    assert!(out.contains("LT=true"), "{}", out);
    assert!(out.contains("checkpoint written"), "{}", out);
    assert!(ck.is_file());

    // Resumed leg: the 20/30 ns reports evaluate deserialized expressions.
    // The clock free-runs, so the resumed leg needs its own time bound.
    let (ok, out) = run(
        &dir,
        &["--restore", ck.to_str().unwrap(), "--max-time", "40ns"],
    );
    assert!(ok, "restore leg must succeed: {}", out);
    assert!(
        out.contains("EQ=true"),
        "resumed EQ must render true: {}",
        out
    );
    assert!(
        out.contains("NE=false"),
        "resumed NE must render false: {}",
        out
    );
    let _ = fs::remove_file(&ck);
    let _ = fs::remove_dir_all(&dir);
}
