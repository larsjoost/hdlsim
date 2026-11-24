-- Self-checking regression fixture for the corpus round 19 front-end fixes
-- (VHDL/Compliance-Tests vhdl_2008 + vhdl_2019):
-- * VHDL-2008 matching case statement with the `end case?` form
--   (ct08_tb_matching_case): parses and runs like a plain case here.
-- * VHDL-2019 sequential block statement (ct19_tb_007): executes its body.
-- * VHDL-2019 subprogram generic clause `generic (type pt) parameter (...)`
--   with `a'subtype` formal/return type marks (ct19_tb_049).
-- * VHDL-2019 `return rv_t of <type>` return form (ct19_tb_072b,
--   ct19_tb_reading_function_return_value).
-- * VHDL-2019 bit-string literal with a `U` base prefix, `6Ux"a"`
--   (ct08_tb_bit_string_literals).
library ieee;
use ieee.std_logic_1164.all;
use std.env.all;

package r19_pkg is
  function pick4
    generic (type pt)
    parameter (s : natural; a : pt; b, c, d : a'subtype)
    return a'subtype;
  function rv_alias return rv_t of std_logic_vector;
end package;

package body r19_pkg is
  function pick4
    generic (type pt)
    parameter (s : natural; a : pt; b, c, d : a'subtype)
    return a'subtype is
    variable rv : a'subtype;
  begin
    case s is
      when 0 => rv := a;
      when 1 => rv := b;
      when 2 => rv := c;
      when others => rv := d;
    end case;
    return rv;
  end function;

  function rv_alias return rv_t of std_logic_vector is
  begin
    return "01";
  end function;
end package body;

use work.r19_pkg.all;

entity tb_r19 is
end entity;

architecture sim of tb_r19 is
  signal q  : std_logic := '0';
  signal r  : std_logic;
  signal bs : bit_vector(5 downto 0);
  constant UBS : bit_vector(5 downto 0) := 6Ux"a";

  function match2(s : natural) return boolean is
  begin
    case? s is
      when 2 | 1 => return true;
      when others => return false;
    end case?;
  end function;
begin
  process
  begin
    -- Sequential block statement (ct19_tb_007).
    inner : block
    begin
      q <= '1';
    end block;
    wait for 1 ns;
    assert q = '1' report "seq block did not assign q" severity error;

    -- Bit-string literal with U prefix (ct08_tb_bit_string_literals).
    bs <= UBS;
    wait for 1 ns;
    assert bs'length = 6 report "U bit-string wrong length" severity error;

    -- Subprogram generic clause + `a'subtype` marks (ct19_tb_049).
    r <= pick4 generic map (pt => std_logic)(1, '0', '1', '0', '0');
    wait for 1 ns;
    assert r = '1' report "pick4 s=1 should return b='1'" severity error;

    -- `return rv_t of std_logic_vector` (ct19_tb_072b).
    assert rv_alias = "01" report "rv_alias should be 01" severity error;

    -- Matching case, `end case?` form (ct08_tb_matching_case).
    assert match2(2) report "match2(2) should be true" severity error;
    assert not match2(0) report "match2(0) should be false" severity error;

    report "PASS: round-19 case?/seq-block/subprog-generic/return-of/U-bitstring";
    std.env.finish;
  end process;
end architecture;
