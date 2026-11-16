-- Self-checking testbench for the `**` operator: constant-folded in
-- constant/generic initializers and vector widths, and evaluated at runtime.
library ieee;
use ieee.std_logic_1164.all;
use std.env.all;

entity tb_pow is
end entity;

architecture sim of tb_pow is
  constant W : natural := 2**3;
  constant DEPTH : natural := 2**2 + 1;
  signal v : std_ulogic_vector(W-1 downto 0);
  signal r : natural := 0;
begin
  v <= x"ff";

  process
    variable acc : natural;
  begin
    acc := 3**2;
    r <= 2**4;
    wait for 1 ns;
    assert acc = 9 report "runtime ** failed" severity error;
    assert r = 16 report "signal ** failed" severity error;
    assert W = 8 report "constant ** failed" severity error;
    assert DEPTH = 5 report "mixed ** failed" severity error;
    assert v'length = W report "** in vector width failed" severity error;
    assert v = x"ff" report "pow-width vector failed" severity error;
    report "PASS: vhdl pow";
    std.env.finish;
    wait;
  end process;
end architecture;
