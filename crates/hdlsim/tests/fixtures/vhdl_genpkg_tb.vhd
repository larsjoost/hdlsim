-- Self-checking testbench for VHDL-2008 generic packages instantiated at
-- design-unit level (`package p is new g generic map (...)`), plus a
-- same-file package used via `use work.pkg.all;` at the top of the file.
library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;
use std.env.all;

package gen_pkg is
  generic (WIDTH : natural := 8);
  constant C_W : natural := WIDTH;
  type vec_t is array (natural range <>) of std_ulogic;
  function c_hi return natural;
end package;

package body gen_pkg is
  function c_hi return natural is
  begin
    return WIDTH - 1;
  end function;
end package body;

package wide_pkg is new work.gen_pkg generic map (WIDTH => 16);

package local_pkg is
  constant C_LOCAL : natural := 4;
end package;

use work.wide_pkg.all;
use work.local_pkg.all;

entity tb_genpkg is
end entity;

architecture sim of tb_genpkg is
  signal v : vec_t(0 to C_W-1);
  signal b : std_ulogic_vector(C_W-1 downto 0) := (others => '0');
  signal l : std_ulogic_vector(C_LOCAL-1 downto 0) := (others => '1');
begin
  process
  begin
    wait for 1 ns;
    assert C_W = 16 report "generic map override not applied" severity error;
    assert c_hi = 15 report "package function did not see the generic" severity error;
    assert v'length = 16 report "instantiated constant not visible" severity error;
    assert C_LOCAL = 4 report "same-file package use failed" severity error;
    b <= (others => '1');
    wait for 1 ns;
    assert to_integer(unsigned(b)) = 65535 report "vector width from generic failed" severity error;
    report "PASS: vhdl generic packages";
    std.env.finish;
    wait;
  end process;
end architecture;
