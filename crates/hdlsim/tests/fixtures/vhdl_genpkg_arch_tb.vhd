-- Self-checking testbench for a VHDL-2008 generic package instantiated in an
-- architecture declarative part (`package p is new g generic map (...)`
-- followed by `use work.p.all;` in the same declarative part).
library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;
use std.env.all;

package gen_pkg is
  generic (WIDTH : natural := 8);
  constant C_W : natural := WIDTH;
  function c_hi return natural;
end package;

package body gen_pkg is
  function c_hi return natural is
  begin
    return WIDTH - 1;
  end function;
end package body;

entity tb_genpkg_arch is
end entity;

architecture sim of tb_genpkg_arch is
  package narrow_pkg is new work.gen_pkg generic map (WIDTH => 4);
  use work.narrow_pkg.all;
  signal v : std_ulogic_vector(C_W-1 downto 0) := (others => '0');
begin
  process
  begin
    wait for 1 ns;
    assert C_W = 4 report "architecture-level instantiation failed" severity error;
    assert c_hi = 3 report "architecture-level package function failed" severity error;
    assert v'length = 4 report "width from arch-level generic failed" severity error;
    report "PASS: vhdl generic packages (arch-level)";
    std.env.finish;
    wait;
  end process;
end architecture;
