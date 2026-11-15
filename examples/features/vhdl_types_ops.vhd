-- bit_vector, signed/unsigned arithmetic, concat, slices, aggregates.
library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;
entity tb is end entity;
architecture sim of tb is
  signal v : std_logic_vector(7 downto 0) := x"A5";
  signal s : signed(7 downto 0) := to_signed(-3, 8);
  signal u : unsigned(7 downto 0) := x"0F";
  signal cat : std_logic_vector(15 downto 0);
  signal hi : std_logic_vector(3 downto 0);
begin
  process
  begin
    cat <= v & x"3C";
    hi <= v(7 downto 4);
    wait for 1 ns;
    if cat = x"A53C" and hi = x"A" and s + 3 = 0 and u + 1 = x"10" and (v and x"0F") = x"05"
      then report "PASS";
      else report "FAIL";
    end if;
    wait;
  end process;
end architecture;
