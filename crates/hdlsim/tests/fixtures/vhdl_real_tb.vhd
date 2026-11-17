-- real / ieee.fixed_pkg / ieee.float_pkg testbench: real literals (incl.
-- exponents), real arithmetic/comparison, integer<->real conversion, and
-- fixed/float types simulated as f64.
library ieee;
use ieee.fixed_pkg.all;
use ieee.float_pkg.all;

entity tb_real is end entity;

architecture sim of tb_real is
  signal r : real := 1.5;
  signal e : real := 2.5e1;          -- exponent literal: 25.0
  signal i : integer;
  signal f : real;
  signal a : sfixed(3 downto -4) := to_sfixed(1.5, 3, -4);
  signal b : ufixed(7 downto 0);
  signal fl : float;
begin
  process
  begin
    wait for 10 ns;
    r <= r + e;                     -- 1.5 + 25.0 = 26.5
    wait for 10 ns;
    i <= integer(r);                -- round to nearest: 27
    wait for 10 ns;
    f <= to_real(i) / 4.0;          -- 6.75
    wait for 10 ns;
    b <= resize(a * 2.0, 7, 0);     -- 3.0 as ufixed
    wait for 10 ns;
    fl <= to_float(-2.5e0);         -- float from real
    wait for 10 ns;
    report "r=" & real'image(r) & " i=" & integer'image(i) & " f=" & real'image(f)
      & " b=" & real'image(to_real(b)) & " fl=" & real'image(to_real(fl));
    if r = 26.5 and i = 27 and f = 6.75 and to_real(b) = 3.0 and to_real(fl) = -2.5
       and a < b and r > 26.0 and to_real(a) /= 0.0 then
      report "PASS: vhdl real/fixed/float";
    else
      report "FAIL: vhdl real/fixed/float";
    end if;
    wait;
  end process;
end architecture;
