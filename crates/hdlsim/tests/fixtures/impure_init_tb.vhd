-- An impure-function constant initializer is legal VHDL evaluated once at
-- elaboration. hdlsim's const-eval cannot execute impure code (the body
-- calls a procedure), so the constant folds to X by design and the
-- constant-initializer warning must stay silent — the value is not a fold
-- gap but genuinely elaboration-time. Both call forms are covered: the
-- parameterless `seeded_vec` and the argument-position `2 * seeded_len`.
package impure_init_style is
  procedure poke(variable dst : out std_ulogic_vector; constant v : in std_ulogic_vector);
end package;

package body impure_init_style is
  procedure poke(variable dst : out std_ulogic_vector; constant v : in std_ulogic_vector) is
  begin
    dst := v;
  end procedure;
end package body;

use work.impure_init_style.all;

entity impure_init_tb is
end entity;

architecture sim of impure_init_tb is
  impure function seeded_vec return std_ulogic_vector is
    variable v : std_ulogic_vector(7 downto 0) := (others => '0');
  begin
    poke(v, "01000001");
    return v;
  end function;

  impure function seeded_len return integer is
    variable v : std_ulogic_vector(7 downto 0) := (others => '0');
  begin
    poke(v, "01000001");
    return v'length + 1;
  end function;

  constant C_SEED  : std_ulogic_vector(7 downto 0) := seeded_vec;
  constant C_TWICE : integer := 2 * seeded_len;
begin
  main : process
  begin
    report "impure_init_tb: C_SEED=" & to_string(C_SEED);
    wait;
  end process;
end architecture;
