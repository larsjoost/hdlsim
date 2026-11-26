-- A pure-function constant initializer that fails to fold keeps the
-- constant-initializer warning: unlike an impure call (expected X by
-- design), a pure fold failure signals a genuine const-eval gap worth
-- reporting. Same shape as impure_init_tb with the `impure` marker
-- dropped.
package pure_init_style is
  procedure poke(variable dst : out std_ulogic_vector; constant v : in std_ulogic_vector);
end package;

package body pure_init_style is
  procedure poke(variable dst : out std_ulogic_vector; constant v : in std_ulogic_vector) is
  begin
    dst := v;
  end procedure;
end package body;

use work.pure_init_style.all;

entity pure_init_fails_tb is
end entity;

architecture sim of pure_init_fails_tb is
  function seeded_vec return std_ulogic_vector is
    variable v : std_ulogic_vector(7 downto 0) := (others => '0');
  begin
    poke(v, "01000001");
    return v;
  end function;

  constant C_SEED : std_ulogic_vector(7 downto 0) := seeded_vec;
begin
  main : process
  begin
    report "pure_init_fails_tb: C_SEED=" & to_string(C_SEED);
    wait;
  end process;
end architecture;
