-- Regression fixture for --trace-assign: a chain of concurrent
-- assignments whose per-evaluation trace (values read, result, whether
-- the value moved) is checkable. `y` reads `a`; `w` reads `y`, so a
-- change to `a` settles through both across fixpoint sweeps.
library ieee;
use ieee.std_logic_1164.all;

entity trace_tb is
end entity;

architecture sim of trace_tb is
  signal a : std_logic_vector(3 downto 0) := "0001";
  signal y : std_logic_vector(3 downto 0);
  signal w : std_logic_vector(3 downto 0);
begin

  y <= a or x"2";
  w <= y;

  main : process
  begin
    wait for 10 ns;
    a <= "0011";
    wait for 10 ns;
    wait;
  end process;

end architecture;
