-- Self-checking testbench for alias declarations: whole-signal, bit-index,
-- and slice aliases must read and write through to the target.
library ieee;
use ieee.std_logic_1164.all;
use std.env.all;

entity tb_alias is
end entity;

architecture sim of tb_alias is
  signal v : std_ulogic_vector(7 downto 0) := x"00";
  signal w : std_ulogic_vector(3 downto 0);
  alias hi  is v(7 downto 4);
  alias lo  is v(3 downto 0);
  alias bit0 is v(0);
  alias all_v is v;
begin
  w <= hi;

  process
  begin
    v <= x"a5";
    wait for 1 ns;
    assert hi = x"a" report "slice alias read failed" severity error;
    assert lo = x"5" report "slice alias lo failed" severity error;
    assert bit0 = '1' report "index alias read failed" severity error;
    assert w = x"a" report "alias in concurrent assign failed" severity error;
    assert all_v = x"a5" report "whole-signal alias failed" severity error;
    -- write through aliases
    hi <= x"3";
    bit0 <= '0';
    wait for 1 ns;
    assert v = x"34" report "alias write failed" severity error;
    report "PASS: vhdl alias";
    std.env.finish;
    wait;
  end process;
end architecture;
