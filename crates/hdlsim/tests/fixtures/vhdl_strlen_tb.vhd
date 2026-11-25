-- Regression: constant folding of string-building subprograms and the
-- `'length` family on their results (LRM 1076 §5.2.1, §7.3.5). Covers:
--   * plain `'length` on a constant/generic (no dimension suffix),
--   * an empty string generic/constant folding to 0,
--   * a constrained string subtype's `'length` counting characters, not bits,
--   * a subprogram local declared `string(1 to size)` (formal-dependent
--     constraint) padded via `(others => ' ')` and a slice assign,
--   * a record-field slice assign inside a subprogram body,
--   * a `report ... severity warning` inside a folded subprogram body.
-- Without the fixes these initializers fell back to X with
-- `warning[W0401]: constant initializer is not constant`.

library ieee;
use ieee.std_logic_1164.all;

package pad_pkg is
  constant C_NAME_LENGTH : integer := 6;
  subtype name_t is string(1 to C_NAME_LENGTH);

  type row_t is array (natural range <>) of integer;

  type opt_t is record
    active : boolean;
    ident  : name_t;
  end record;

  constant C_DEFAULT_OPT : opt_t :=
    (active => false, ident => (others => ' '));

  -- Pads arg with spaces to size characters.
  function pad(arg : string; constant size : integer) return string;

  -- Builds a record via field and field-slice assignments.
  function set_opt(arg : string) return opt_t;

  -- Log2 shape: warns on non-power-of-two input, still folds.
  function ceil_log2(n : positive) return natural;
end package;

package body pad_pkg is
  function pad(arg : string; constant size : integer) return string is
    variable result : string(1 to size) := (others => ' ');
  begin
    if (arg'length > 0) then
      result(1 to arg'length) := arg;
    end if;
    return result;
  end function;

  function set_opt(arg : string) return opt_t is
    variable result : opt_t := C_DEFAULT_OPT;
  begin
    result.active := true;
    result.ident  := pad(arg, name_t'length);
    return result;
  end function;

  function ceil_log2(n : positive) return natural is
    variable result : natural := 0;
    variable v      : positive := n;
  begin
    while v > 1 loop
      if (v mod 2 = 1) then
        report "input is not a power of two" severity warning;
      end if;
      v := (v + 1) / 2;
      result := result + 1;
    end loop;
    return result;
  end function;
end package body;

use work.pad_pkg.all;

entity strlen_tb is
  generic (
    G_TAG  : string := "";
    G_NAME : name_t := pad("ab", C_NAME_LENGTH));
end entity;

architecture sim of strlen_tb is
  constant C_EMPTY_GEN : natural := G_TAG'length;
  constant C_NOFILE    : string  := "";
  constant C_EMPTY_CON : natural := C_NOFILE'length;
  constant C_NAME_LEN  : natural := G_NAME'length;
  constant C_SUB_LEN   : natural := name_t'length;
  constant C_ROW       : row_t   := (1, 2, 3, 4, 5);
  constant C_ROW_LEN   : natural := C_ROW'length;
  constant C_OPT       : opt_t   := set_opt("hi");
  constant C_LOG2      : natural := ceil_log2(12);
begin
  main : process
  begin
    report "EMPTY_GEN=" & integer'image(C_EMPTY_GEN);
    report "EMPTY_CON=" & integer'image(C_EMPTY_CON);
    report "NAME_LEN=" & integer'image(C_NAME_LEN);
    report "SUB_LEN=" & integer'image(C_SUB_LEN);
    report "ROW_LEN=" & integer'image(C_ROW_LEN);
    report "OPT_ACTIVE=" & boolean'image(C_OPT.active);
    report "LOG2=" & integer'image(C_LOG2);
    wait;
  end process;
end architecture;
