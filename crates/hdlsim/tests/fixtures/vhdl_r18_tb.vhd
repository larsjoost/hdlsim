-- Self-checking regression fixture for the corpus round 18 front-end fixes
-- (GHDL testsuite synth tests):
-- * VHDL-2008 `unaffected` waveform element (asgn01/asgn10, issue2361/jkff):
--   sequential `q <= unaffected;` and a concurrent conditional waveform with
--   a trailing `else unaffected` both keep the current value.
-- * Box generic `T : type` (issue412/generic_sfifo): registers T as a type.
-- * External-name alias `<< signal ^.path : t >>` (external01/external03):
--   lowers to a placeholder so uses of the alias resolve.
-- * VHDL-2019 math_real constants (issue2176/math_real_test): MATH_SQRT_PI.
library ieee;
use ieee.std_logic_1164.all;
use ieee.math_real.all;
use std.env.all;

entity r18_box is
  generic (
    T : type;
    N : natural := 4
  );
  port (
    d_in  : in  T;
    d_out : out T
  );
end entity;

architecture rtl of r18_box is
begin
  d_out <= d_in;
end architecture;

entity tb_r18 is
end entity;

architecture sim of tb_r18 is
  signal q          : std_logic := '0';
  signal o          : std_logic_vector(1 downto 0) := "00";
  signal a, b       : std_logic_vector(1 downto 0) := "00";
  signal sel_a, sel_b : std_logic := '0';
  signal ext_use    : std_logic;
  constant SQRT_PI_C : real := MATH_SQRT_PI;
  -- External-name alias with an absolute path (leading `.`), as in
  -- external01/external03's `<< signal .external03.dut.accum : ... >>`.
  alias ext_q is << signal .tb_r18.q : std_logic >>;
begin
  -- Concurrent conditional waveform, trailing `else unaffected` (asgn10).
  o <= a when sel_a = '1' else
       b when sel_b = '1' else
       unaffected;

  process
  begin
    -- Sequential `unaffected` (jkff): no transaction, q keeps its value.
    q <= '1';
    wait for 1 ns;
    assert q = '1' report "q should be '1'" severity error;
    q <= unaffected;
    wait for 1 ns;
    assert q = '1' report "unaffected changed q" severity error;
    ext_use <= ext_q;  -- placeholder alias: must lower without error

    -- Conditional waveform branches (asgn10).
    a <= "01"; sel_a <= '1';
    wait for 1 ns;
    assert o = "01" report "o should be 01" severity error;
    sel_a <= '0'; sel_b <= '1'; b <= "10";
    wait for 1 ns;
    assert o = "10" report "o should be 10" severity error;
    -- Neither condition: `else unaffected` keeps o.
    sel_a <= '0'; sel_b <= '0';
    wait for 1 ns;
    assert o = "10" report "else unaffected did not keep o" severity error;

    -- math_real VHDL-2019 constant (issue2176).
    assert SQRT_PI_C > 1.77 and SQRT_PI_C < 1.78
      report "MATH_SQRT_PI out of range" severity error;

    report "PASS: round-18 unaffected/box-generic/extname/math_real";
    std.env.finish;
  end process;
end architecture;
