library ieee;
use ieee.std_logic_1164.all;
use std.env.all;

entity child is
  port (a : in std_ulogic_vector; y : out std_ulogic);
end entity;

architecture rtl of child is
begin
  y <= a(a'high) and a(a'length - 1);
end architecture;

entity tb_unconport is
end entity;

architecture sim of tb_unconport is
  signal v : std_ulogic_vector(3 downto 0) := "1010";
  signal o : std_ulogic;
begin
  u: entity work.child port map (a => v, y => o);
  process
  begin
    wait for 1 ns;
    assert o = '1' report "unconstrained port failed" severity error;
    report "PASS: vhdl unconstrained port";
    std.env.finish;
    wait;
  end process;
end architecture;
