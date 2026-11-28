-- Fixture: scalar metavalue equality on std_logic literals (IEEE 1164,
-- predefined relational operators). Metavalues are ordinary values to the
-- predefined '='/'/=': 'Z' differs from '0', equals itself, and the
-- don't-care literal '-' is a distinct value — never a wildcard and never
-- an unknown result.
library ieee;
use ieee.std_logic_1164.all;
entity tb_scalar_meta_eq is
end entity;

architecture sim of tb_scalar_meta_eq is
begin
  main : process
  begin
    report "('Z' = '0')  is " & boolean'image('Z' = '0');
    report "('Z' /= '0') is " & boolean'image('Z' /= '0');
    report "('Z' = 'Z')  is " & boolean'image('Z' = 'Z');
    report "('-'  = '0')  is " & boolean'image('-' = '0');
    assert ('Z' = '0') = false
      report "'Z'='0' must be false" severity failure;
    assert ('Z' /= '0') = true
      report "'Z'/='0' must be true" severity failure;
    assert ('Z' = 'Z') = true
      report "'Z'='Z' must be true" severity failure;
    assert ('-' = '0') = false
      report "'-'='0' must be false" severity failure;
    report "PASS scalar_meta_eq";
    wait;
  end process;
end architecture;
