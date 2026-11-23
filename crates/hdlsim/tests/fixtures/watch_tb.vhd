-- Regression fixture for HDLSIM_WATCH: a small sequence of signal
-- changes with a known transition into metavarues, so the change
-- lines, x-only filter, snapshots, and the line cap are all checkable.
library ieee;
use ieee.std_logic_1164.all;

entity watch_tb is
end entity;

architecture sim of watch_tb is
  signal a : std_logic_vector(3 downto 0) := "0001";
  signal q : std_logic_vector(3 downto 0) := "0000";
begin

  main : process
  begin
    q <= a;                -- clean change at t=0
    wait for 10 ns;
    q <= "XXXX";           -- transition into metavarues at t=10 ns
    wait for 10 ns;
    q <= a;                -- clean change back at t=20 ns
    wait for 10 ns;
    wait;
  end process;

end architecture;
