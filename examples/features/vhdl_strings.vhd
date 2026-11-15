-- String functions: to_hstring / to_ostring / to_bstring / to_string, concat.
library ieee;
use ieee.std_logic_1164.all;
entity tb is end entity;
architecture sim of tb is
  signal v : std_logic_vector(7 downto 0) := x"A5";
begin
  process
  begin
    if to_hstring(v) = "A5" and to_ostring(v) = "245" and to_bstring(v) = "10100101"
       and to_string(v) = to_bstring(v) and ("h" & to_hstring(v)) = "hA5"
      then report "PASS";
      else report "FAIL";
    end if;
    wait;
  end process;
end architecture;
