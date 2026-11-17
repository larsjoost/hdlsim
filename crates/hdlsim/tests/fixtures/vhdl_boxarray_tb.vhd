library ieee;
use ieee.std_logic_1164.all;
use std.env.all;

entity tb_boxarray is
end entity;

architecture sim of tb_boxarray is
  type mem_t is array (range <>, range <>) of std_ulogic_vector(7 downto 0);
  signal m : mem_t(0 to 3, 0 to 1);
begin
  process
  begin
    m(1, 0) <= x"aa";
    m(3, 1) <= x"5c";
    wait for 1 ns;
    assert m(1, 0) = x"aa" report "m(1,0) failed" severity error;
    assert m(3, 1) = x"5c" report "m(3,1) failed" severity error;
    assert m'length = 8 and m'high = 7 report "box array attrs failed" severity error;
    report "PASS: vhdl box array";
    std.env.finish;
    wait;
  end process;
end architecture;
