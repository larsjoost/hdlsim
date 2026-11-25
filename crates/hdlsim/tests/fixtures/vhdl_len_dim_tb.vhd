-- Regression: `'length(d)` / `'high(d)` / `'low(d)` with an explicit
-- dimension on multidimensional arrays (LRM 1076 §5.2.1). Covers the three
-- dim sources: an aggregate-typed generic default, a package constant with
-- an inline decl constraint (dims cross files with the package), and an
-- array-typed signal. Bare `'length` (dimension 1) stays the control.

library ieee;
use ieee.std_logic_1164.all;

package grid_pkg is
  type grid_t is array (natural range <>, natural range <>) of integer;

  -- Inline decl constraint: the dims are the object's, not the
  -- unconstrained base type's — and they must cross files with the package.
  constant C_ORIGIN : grid_t(0 to 1, 0 to 2) := ((1, 2, 3), (4, 5, 6));
end package;

use work.grid_pkg.all;

entity len_dim_tb is
  generic (
    G_GRID : grid_t := ((1, 2, 3), (4, 5, 6)));
end entity;

architecture sim of len_dim_tb is
  constant C_ROWS  : natural := G_GRID'length(1);
  constant C_COLS  : natural := G_GRID'length(2);
  constant C_MULTI : natural := 1 when G_GRID'length(1) > 1 else 0;

  constant C_ORIG_ROWS : natural := C_ORIGIN'length(1);
  constant C_ORIG_COLS : natural := C_ORIGIN'length(2);

  signal plane_s : grid_t(0 to 4, 0 to 1) := (others => (others => 0));

  -- Guard: a scalar dimension suffix on a 1-D vector is still dimension 1.
  constant C_VEC_LEN : natural := G_GRID'length(1);
begin
  main : process
    variable v_len : natural;
  begin
    v_len := plane_s'length(2);
    report "ROWS=" & integer'image(C_ROWS);
    report "COLS=" & integer'image(C_COLS);
    report "MULTI=" & boolean'image(C_MULTI);
    report "ORIG_ROWS=" & integer'image(C_ORIG_ROWS);
    report "ORIG_COLS=" & integer'image(C_ORIG_COLS);
    report "SIG_COLS=" & integer'image(v_len);
    report "VEC_LEN=" & integer'image(C_VEC_LEN);
    wait;
  end process;
end architecture;
