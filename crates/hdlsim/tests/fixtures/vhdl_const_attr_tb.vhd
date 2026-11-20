-- Self-checking testbench for const-evaluated attributes/conversions in
-- constant initializers: `type'val`, `type'pos`, `T'image`, `to_integer`.
-- These used to warn "constant initializer is not constant; treated as X".
library ieee;
use ieee.std_logic_1164.all;
use std.env.all;

package const_attr_pkg is
  constant A : character := character'val(65);          -- 'A'
  constant B : integer := integer'val(7);
  constant C : integer := character'pos(character'val(66));  -- 66
  constant S : string(1 to 2) := integer'image(42);      -- "42"
  constant D : integer := to_integer(9);
end package;

entity tb_const_attr is
end entity;

architecture sim of tb_const_attr is
begin
  process
  begin
    assert A = character'val(65) report "character'val wrong" severity error;
    assert character'pos(A) = 65 report "character'pos wrong" severity error;
    assert B = 7 report "integer'val wrong" severity error;
    assert C = 66 report "'pos/'val chain wrong" severity error;
    assert S = "42" report "integer'image wrong: " & S severity error;
    assert D = 9 report "to_integer in constant wrong" severity error;
    report "PASS: vhdl const attr/val/image";
    std.env.finish;
  end process;
end architecture;
