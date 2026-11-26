-- An impure-function constant initializer is legal VHDL, evaluated once at
-- elaboration. hdlsim's const-eval executes the body (procedure calls run
-- with inout/out copy-back), so an effect-free impure function folds to its
-- real value. What the const-eval cannot know — a signal read — falls back
-- to X, and the constant-initializer warning must stay silent either way:
-- the value is not a fold gap but genuinely elaboration-time.
package impure_init_style is
  procedure bump(variable x : inout integer);
end package;

package body impure_init_style is
  procedure bump(variable x : inout integer) is
  begin
    x := x + 40;
  end procedure;
end package body;

use work.impure_init_style.all;

entity impure_init_tb is
end entity;

architecture sim of impure_init_tb is
  signal s_live : integer := 0;

  impure function seeded_int return integer is
    variable v : integer := 0;
  begin
    bump(v);
    return v + 2;
  end function;

  impure function live_int return integer is
  begin
    return s_live;  -- a signal read: no constant value exists
  end function;

  constant C_SEED : integer := seeded_int;  -- effect-free: folds to 42
  constant C_LIVE : integer := live_int;    -- signal read: X, silently
begin
  main : process
  begin
    report "impure_init_tb: C_SEED=" & integer'image(C_SEED);
    report "impure_init_tb: C_LIVE=" & integer'image(C_LIVE);
    wait;
  end process;
end architecture;
