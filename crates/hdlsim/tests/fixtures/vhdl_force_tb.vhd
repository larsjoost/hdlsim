-- Self-checking testbench for VHDL-2008 force/release signal assignments:
-- `s <= force '1';`, `s <= release '0';`, and force on one waveform element
-- of a multi-element waveform (`s <= force '1' after 5 ns, '0' after 10 ns;`).
-- Lexing note: `force`/`release` are reserved words, so the tick in the
-- following `'1'` must lex as a char literal, not an attribute tick.
library ieee;
use ieee.std_logic_1164.all;
use std.env.all;

entity tb_force is
end entity;

architecture sim of tb_force is
  signal s : std_logic := '0';
begin
  process
  begin
    s <= force '1';
    wait for 1 ns;
    assert s = '1' report "force '1' wrong: " & std_logic'image(s) severity error;
    s <= release '0';
    wait for 1 ns;
    assert s = '0' report "release '0' wrong: " & std_logic'image(s) severity error;
    s <= force '1' after 2 ns, '0' after 4 ns;
    wait for 3 ns;
    assert s = '1' report "force after wrong: " & std_logic'image(s) severity error;
    wait for 2 ns;
    assert s = '0' report "second element wrong: " & std_logic'image(s) severity error;
    report "PASS: vhdl-2008 force/release assignments";
    std.env.finish;
  end process;
end architecture;
