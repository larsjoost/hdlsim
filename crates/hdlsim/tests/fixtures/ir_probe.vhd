-- Fixture for the `hdlsim ir` dump: one entity + architecture (the
-- entity/arch pair must auto-collapse to a single --top candidate), one
-- signal of each of the value shapes the JSON must represent (1-bit,
-- bus, real), and a process so `procs` is non-empty.
library ieee;
use ieee.std_logic_1164.all;

entity ir_probe is
end entity;

architecture rtl of ir_probe is
  signal clk : std_logic := '0';
  signal cnt : std_logic_vector(7 downto 0) := x"2A";
  signal temp : real := 0.0;
begin
  drive : process
  begin
    clk <= not clk;
    wait for 5 ns;
  end process;

  sample : process(clk)
  begin
    if rising_edge(clk) then
      temp <= real(to_integer(unsigned(cnt)));
    end if;
  end process;
end architecture;
