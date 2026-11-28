-- Fixture: predefined equality on strings (LRM 1076, relational operators).
-- An all-0/1 string literal lowers to a bit-packed value while to_bstring
-- returns byte-packed characters; `=` must reconcile the two shapes in both
-- directions, genuinely different strings must stay unequal, and a selected
-- assignment (whose choices compare strictly, without the loose-op rewrite)
-- must match across the two shapes too.
library ieee;
use ieee.std_logic_1164.all;
entity tb_string_eq is
end entity;

architecture sim of tb_string_eq is
  signal v   : std_logic_vector(7 downto 0) := x"A5";
  signal sel : std_logic;
begin
  -- Choice compares strictly: the byte-packed selector must reconcile with
  -- the bit-spelled choice literal.
  with to_bstring(v) select
    sel <= '1' when "10100101",
           '0' when others;

  main : process
  begin
    assert to_bstring(v) = "10100101"
      report "to_bstring(v) = ""10100101"" must be true" severity failure;
    assert to_bstring(v) /= "00000000"
      report "to_bstring(v) /= ""00000000"" must be true" severity failure;
    assert "10100101" = to_bstring(v)
      report "literal on the left must compare too" severity failure;
    assert v = "10100101"
      report "slv vs bit-spelled literal" severity failure;
    assert sel = '1'
      report "selected assignment must match the bit-spelled choice"
        severity failure;
    report "PASS string_eq";
    wait;
  end process;
end architecture;
