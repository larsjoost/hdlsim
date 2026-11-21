-- Self-checking testbench: instantiation without a generic map must pick up
-- the entity's aggregate default for the record-typed generic, and the
-- sibling-file architecture must read its fields.
library ieee;
use ieee.std_logic_1164.all;
use std.env.all;

entity tb_cross_generic is
end entity;

architecture sim of tb_cross_generic is
  signal d : std_logic := '0';
  signal q : std_logic;
begin
  d <= '0', '1' after 3 ns;

  u1 : entity work.cg_blk
    port map (d => d, q => q);

  check : process
  begin
    wait for 1 ns;
    assert q = '0' report "FAIL: q should follow d at 1 ns" severity error;
    wait for 4 ns;
    assert q = '1' report "FAIL: q should follow d at 5 ns" severity error;
    report "PASS: cross-file record generic field selects resolve";
    wait;
  end process;
end architecture;
