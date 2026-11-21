-- Self-checking testbench: the generic-mapped instance (a variant) must get
-- the sibling-file architecture body; the unmapped instance keeps defaults.
library ieee;
use ieee.std_logic_1164.all;
use std.env.all;

entity tb_variant_arch is
end entity;

architecture sim of tb_variant_arch is
  signal d1 : std_logic := '0';
  signal q1 : std_logic;
  signal d2 : std_logic := '0';
  signal q2 : std_logic;
begin
  d1 <= '1' after 3 ns;
  d2 <= '1' after 3 ns;

  u1 : entity work.va_blk
    generic map (G_INV => true)
    port map (d => d1, q => q1);

  u2 : entity work.va_blk
    port map (d => d2, q => q2);

  check : process
  begin
    wait for 1 ns;
    assert q1 = '1' report "FAIL: variant q1 should invert d1" severity error;
    assert q2 = '0' report "FAIL: default q2 should follow d2" severity error;
    wait for 4 ns;
    assert q1 = '0' report "FAIL: variant q1 after d1 toggle" severity error;
    assert q2 = '1' report "FAIL: default q2 after d2 toggle" severity error;
    report "PASS: generic-map variant re-lowered with sibling architecture";
    wait;
  end process;
end architecture;
