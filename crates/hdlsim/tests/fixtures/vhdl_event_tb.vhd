-- Self-checking testbench for the classic `clk'event and clk = '1'` clock
-- idiom (attribute 'event was previously X) and `wait on` sensitivity.
library ieee;
use ieee.std_logic_1164.all;
use std.env.all;

entity tb_event is
end entity;

architecture sim of tb_event is
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
    if clk'event and clk = '1' then
      cnt <= cnt + 1;
    end if;
    if cnt = 2 then
      report "PASS: vhdl event";
      std.env.finish;
    end if;
    wait on clk;
  end process;
end architecture;
