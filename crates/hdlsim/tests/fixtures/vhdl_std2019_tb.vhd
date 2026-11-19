-- Self-checking testbench for the embedded VHDL-2008/2019 standard units:
-- `context ieee.ieee_std_context;`, `ieee.numeric_std_unsigned` (incl.
-- `to_slv`), and `ieee_proposed.standard_additions` (`to_string` of a real).
context ieee.ieee_std_context;
library ieee_proposed;
use ieee_proposed.standard_additions.all;
use ieee.numeric_std_unsigned.all;
use std.env.all;

entity tb_std2019 is
end entity;

architecture sim of tb_std2019 is
  signal u : unsigned(7 downto 0) := x"A5";
  signal v : std_logic_vector(7 downto 0) := x"00";
begin
  process
  begin
    v <= to_slv(u + 1);
    wait for 1 ns;
    assert v = x"A6" report "to_slv(u+1) wrong: " & to_hstring(v) severity error;
    assert to_string(2.5) = "2.5" report "to_string(real) wrong: " & to_string(2.5) severity error;
    report "PASS: vhdl std_context/numeric_std_unsigned/standard_additions";
    std.env.finish;
  end process;
end architecture;
