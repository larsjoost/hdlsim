-- Fixture for the `--dump-consts` end-to-end tests: one known constant, a
-- string constant, a generic with a default (overridden in one instance and
-- defaulted in the other), an unset generic, a constant the constant
-- evaluator folds (real exponentiation), and one it cannot (a procedure
-- call in the initializer).
library ieee;
use ieee.std_logic_1164.all;

package dmp_pkg is
  constant C_ANSWER : integer := 42;
  constant C_STR    : string  := "ok";
end package;

package body dmp_pkg is
end package body;

use work.dmp_pkg.all;

entity dmp_unit is
  generic (
    G_W : positive := 8;
    G_X : positive
  );
  port (o : out std_ulogic_vector(0 to G_W - 1));
end entity;

architecture rtl of dmp_unit is
  constant C_ROOT2 : real := 2.0 ** 0.5;

  procedure setr(variable r : out natural) is
  begin
    r := 1;
  end procedure;

  function undependable return natural is
    variable r : natural := 0;
  begin
    setr(r);
    return r;
  end function;

  constant C_UNFOLDABLE : natural := undependable;
begin
  o <= (others => '0');
end architecture;

entity dmp_tb is
end entity;

architecture sim of dmp_tb is
  signal a : std_ulogic_vector(0 to 15);
  signal b : std_ulogic_vector(0 to 7);
begin
  u1 : entity work.dmp_unit generic map (G_W => 16) port map (o => a);
  u2 : entity work.dmp_unit port map (o => b);
end architecture;
