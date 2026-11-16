-- Self-checking testbench for VHDL shift/rotate operators and their
-- std_logic_arith function forms: sll/srl/sla/sra/rol/ror and
-- shift_left/shift_right/rotate_left/rotate_right on unsigned and signed.
library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;
use ieee.std_logic_arith.all;
use std.env.all;

entity tb_shifts is
end entity;

architecture sim of tb_shifts is
begin
  process
    variable u : unsigned(7 downto 0);
    variable s : signed(7 downto 0);
  begin
    u := x"0F";                       -- 0000 1111
    assert (u sll 1) = x"1E" report "sll failed" severity error;
    assert (u srl 1) = x"07" report "srl failed" severity error;
    assert (u sla 1) = x"1E" report "sla failed" severity error;
    assert (u rol 4) = x"F0" report "rol failed" severity error;
    assert (u ror 4) = x"F0" report "ror failed" severity error;
    assert shift_left(u, 2) = x"3C" report "shift_left failed" severity error;
    assert shift_right(u, 2) = x"03" report "shift_right failed" severity error;
    assert rotate_left(u, 1) = x"1E" report "rotate_left failed" severity error;
    assert rotate_right(u, 1) = x"87" report "rotate_right failed" severity error;

    s := x"F0";                       -- -16 signed
    assert (s sra 1) = x"F8" report "sra failed" severity error;   -- -8
    assert (s srl 1) = x"78" report "srl-on-signed failed" severity error; -- logical
    assert to_integer(s sra 4) = -1 report "sra sign failed" severity error;

    report "PASS: vhdl shifts";
    std.env.finish;
    wait;
  end process;
end architecture;
