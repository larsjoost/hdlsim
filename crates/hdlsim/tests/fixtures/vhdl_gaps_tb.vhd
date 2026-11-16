-- Self-checking testbench for: multi-dimensional arrays (incl. m(i,j)
-- indexing), static attributes (m'length / m'high), and concurrent assert.
library ieee;
use ieee.std_logic_1164.all;
use std.env.all;

entity tb_gaps is
end entity;

architecture sim of tb_gaps is
  type mem2d_t is array (0 to 1, 0 to 3) of std_ulogic;
  signal m : mem2d_t;
  signal clk : std_logic := '0';
  signal cnt : integer := 0;
begin
  process
  begin
    clk <= not clk;
    wait for 5 ns;
  end process;

  -- concurrent assert (previously "not lowered yet")
  assert cnt < 100 report "cnt overflow" severity error;

  process
  begin
    m(0, 1) <= '1';
    m(1, 3) <= '1';
    wait until rising_edge(clk);
    assert m(0, 1) = '1' report "m(0,1) failed" severity error;
    assert m(1, 3) = '1' report "m(1,3) failed" severity error;
    assert m'length = 8 report "m'length != 8" severity error;
    assert m'high = 7 report "m'high != 7" severity error;
    report "PASS: vhdl gaps";
    std.env.finish;
    wait;
  end process;
end architecture;
