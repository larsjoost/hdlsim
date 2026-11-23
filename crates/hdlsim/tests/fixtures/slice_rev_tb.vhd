-- Regression fixture: a signal assignment whose LHS is a slice with
-- reversed *dynamic* bounds. The write path must clamp the slice like the
-- read path (Value::slice) instead of letting `msb - lsb` wrap to
-- usize::MAX and spin forever inside one assign call.
library ieee;
use ieee.std_logic_1164.all;

entity slice_rev_tb is
end entity;

architecture sim of slice_rev_tb is
  signal a : std_logic_vector(3 downto 0) := "0001";
  signal q : std_logic_vector(3 downto 0) := "0000";
  signal j : integer := 1;
begin

  main : process
  begin
    q(0 downto j) <= a(0 downto j);
    wait for 1 ns;
    if q(0) = '1' then
      report "slice_rev PASS";
    else
      report "slice_rev FAIL";
    end if;
    wait;
  end process;

end architecture;
