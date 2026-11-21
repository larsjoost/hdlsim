-- Self-checking testbench for named association lists in generic/port maps.
-- The lexer emits the `=>` arrow as two punctuation tokens; the lowerer must
-- recognise the adjacent pair, otherwise every named association degrades to
-- positional: generic actuals leak into the port connections and shift them.
library ieee;
use ieee.std_logic_1164.all;
use std.env.all;

entity na_dut is
  generic (
    G_W   : integer := 4;
    G_INV : boolean := false);
  port (
    d      : in  std_logic_vector(G_W-1 downto 0);
    sreset : in  std_logic := '0';
    q      : out std_logic_vector(G_W-1 downto 0));
end entity;

architecture rtl of na_dut is
begin
  q <= not d when G_INV else d;
end architecture;

entity tb_named_assoc is
end entity;

architecture sim of tb_named_assoc is
  signal d1 : std_logic_vector(7 downto 0) := x"3C";
  signal q1 : std_logic_vector(7 downto 0);
  signal d2 : std_logic_vector(3 downto 0) := "0101";
  signal q2 : std_logic_vector(3 downto 0);
begin
  -- Named associations in a different order than the declarations, with an
  -- unconnected input that has an entity default (sreset).
  u1 : entity work.na_dut
    generic map (
      G_INV => true,
      G_W   => 8)
    port map (
      q  => q1,
      d  => d1);

  u2 : entity work.na_dut
    generic map (
      G_W   => 4,
      G_INV => false)
    port map (
      d  => d2,
      q  => q2);

  check : process
  begin
    wait for 1 ns;
    if q1 = x"C3" and q2 = "0101" then
      report "PASS: named generic/port associations bind correctly";
    else
      report "FAIL: named association binding" severity error;
    end if;
    wait;
  end process;
end architecture;
