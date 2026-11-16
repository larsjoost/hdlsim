library ieee;
use ieee.std_logic_1164.all;
use std.env.all;
use work.mypkg.all;

entity tb_constfn is
end entity;

architecture sim of tb_constfn is
  constant C  : natural := sel_f(true, 7, 3);
  constant C2 : natural := sel_f(false, 7, 3);
  constant W  : natural := bits_f(8);
  signal v : std_ulogic_vector(W-1 downto 0);
begin
  v <= (others => '1');

  process
  begin
    wait for 1 ns;
    assert C = 7 report "sel_f(true) failed" severity error;
    assert C2 = 3 report "sel_f(false) failed" severity error;
    assert W = 3 report "bits_f(8) failed" severity error;
    assert v'length = 3 and v = "111" report "const-fn width failed" severity error;
    report "PASS: vhdl const function calls";
    std.env.finish;
    wait;
  end process;
end architecture;
