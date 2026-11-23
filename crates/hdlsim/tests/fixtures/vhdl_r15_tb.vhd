-- Self-checking testbench for the VHDL-2019 conditional expression with an
-- omitted final `else` (`v when c`, LRM 1076-2019 §9.2.9): the missing else
-- branch means 'others => X'. Also covers the 2019 chained form
-- `v when c1 else w when c2` (else branch is itself a conditional expression).
library ieee;
use ieee.std_logic_1164.all;
use std.env.all;

entity tb_r15 is
  -- A user physical type with custom units (GHDL sanity_004all08 style):
  -- physical literals (`1.5 cm`) and bare unit names (`mm`) must lower to
  -- base-unit integers.
  type dist_t is range 0 to 1e16 units
    angstrom;
    nm = 10 angstrom;
    um = 1000 nm;
    mm = 1000 um;
    cm = 10 mm;
  end units;
end entity;

architecture sim of tb_r15 is
  signal s : std_logic := '0';

  -- VHDL-2019: function result via conditional expression without else.
  function inv_mode(m : integer) return std_logic is
  begin
    return '1' when m = 2 or m = 3;
  end function;
begin
  process
    variable v   : std_logic;
    variable sel : integer := 0;
    variable d   : dist_t;
  begin
    -- No final else, condition true -> true branch.
    v := '1' when sel = 0;
    assert v = '1' report "no-else cond (true) wrong: " & std_logic'image(v) severity error;

    -- Classic 2008 form with else still works.
    v := '0' when sel = 1 else '1';
    assert v = '1' report "else cond wrong: " & std_logic'image(v) severity error;

    -- 2019 chained form: `else` followed by another `when` without final else.
    sel := 2;
    v := '0' when sel = 1 else '1' when sel = 2;
    assert v = '1' report "chained no-else cond wrong: " & std_logic'image(v) severity error;

    -- Function returning a no-else conditional expression.
    assert inv_mode(3) = '1' report "func no-else cond wrong" severity error;

    -- Concurrent conditional waveform (waveform position, unchanged) plus a
    -- parenthesized conditional expression as the waveform value.
    s <= '1' when sel = 2 else '0';
    wait for 1 ns;
    assert s = '1' report "cond waveform wrong: " & std_logic'image(s) severity error;

    -- Physical type: literal with a user unit in a conditional expression,
    -- and a bare unit name as the else value (in base units: 1 cm = 1e7
    -- angstrom, 1 mm = 1e6 angstrom).
    d := 1.5 cm when sel >= 0 else mm;
    assert d = 1.5 cm report "phys cond literal wrong" severity error;
    d := 0 cm when sel < 0 else mm;
    assert d = mm report "bare physical unit name wrong" severity error;

    report "PASS: vhdl-2019 conditional expression without final else";
    std.env.finish;
  end process;
end architecture;
