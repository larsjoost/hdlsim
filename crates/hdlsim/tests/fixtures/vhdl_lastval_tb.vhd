-- Self-checking testbench for 'last_value and 'stable attributes.
library ieee;
use ieee.std_logic_1164.all;
use std.env.all;

entity tb_lastval is
end entity;

architecture sim of tb_lastval is
  signal clk : std_logic := '0';
  signal cnt : integer := 0;
begin
  process
  begin
    clk <= not clk;
    wait for 5 ns;
  end process;

  process
  begin
    wait until rising_edge(clk);
    -- at a rising edge, the previous clock value was '0' and an event occurred
    assert clk'last_value = '0' report "clk'last_value != 0 at rising edge" severity error;
    assert not clk'stable report "clk'stable should be false on an event" severity error;
    cnt <= cnt + 1;
    if cnt = 2 then
      report "PASS: vhdl last_value/stable";
      std.env.finish;
    end if;
    wait until rising_edge(clk);
  end process;
end architecture;
