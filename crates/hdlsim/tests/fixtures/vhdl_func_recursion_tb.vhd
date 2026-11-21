-- Infinite recursion must stop the simulator with a diagnostic, not overflow
-- the native stack.
library ieee;
use ieee.std_logic_1164.all;

entity tb_func_recursion is
end entity;

architecture sim of tb_func_recursion is
  function deep(n : integer) return integer is
  begin
    return deep(n + 1);
  end function;
begin
  check : process
  begin
    report "unreachable: " & integer'image(deep(0));
    wait;
  end process;
end architecture;
