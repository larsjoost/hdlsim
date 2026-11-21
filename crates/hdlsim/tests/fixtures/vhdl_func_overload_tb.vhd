-- Overloaded functions (same name, different argument counts) must be
-- matched by arity at the call site, not by name alone.
library ieee;
use ieee.std_logic_1164.all;
use std.env.all;

entity tb_func_overload is
end entity;

architecture sim of tb_func_overload is
  function bump(a : integer) return integer is
  begin
    return a + 1;
  end function;

  function bump(a : integer; b : integer) return integer is
  begin
    return a + b;
  end function;
begin
  check : process
    variable r1 : integer;
    variable r2 : integer;
  begin
    r1 := bump(1);
    r2 := bump(1, 2);
    if r1 = 2 and r2 = 3 then
      report "PASS: overloaded functions matched by arity";
    else
      report "FAIL: overloaded function selection" severity error;
    end if;
    wait;
  end process;
end architecture;
