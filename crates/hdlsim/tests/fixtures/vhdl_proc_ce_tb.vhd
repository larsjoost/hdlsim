-- Const-eval of user procedure call statements: a procedure called from a
-- constant initializer's function runs at elaboration, and its `inout`/`out`
-- formals copy their final values back to the caller's variables (LRM 1076
-- §4.3) — previously every such initializer folded to X (W0401). Scalar
-- cases; array-element copy-back lands with the shape-at-binding work.
package proc_ce_pk is
  procedure bump(variable x : inout integer);
  procedure triple(variable o : out integer; base : in integer);
  procedure step_by(variable x : inout integer; step : in integer := 2);
  procedure inner(variable n : inout integer);
  procedure outer(variable n : inout integer);
  procedure sum_to(variable acc : inout integer; n : in integer);
  procedure spin(variable n : inout integer);  -- self-recursive
end package;

package body proc_ce_pk is
  procedure bump(variable x : inout integer) is
  begin
    x := x + 1;
  end procedure;

  procedure triple(variable o : out integer; base : in integer) is
  begin
    o := base * 3;
  end procedure;

  procedure step_by(variable x : inout integer; step : in integer := 2) is
  begin
    x := x + step;
  end procedure;

  procedure inner(variable n : inout integer) is
  begin
    n := n * 2;
  end procedure;

  procedure outer(variable n : inout integer) is
  begin
    inner(n);
    n := n + 1;
  end procedure;

  procedure sum_to(variable acc : inout integer; n : in integer) is
  begin
    for k in 1 to n loop
      acc := acc + k;
    end loop;
  end procedure;

  -- A self-recursive procedure must abort the fold, not hang elaboration.
  procedure spin(variable n : inout integer) is
  begin
    n := n + 1;
    spin(n);
  end procedure;
end package body;

use work.proc_ce_pk.all;

entity proc_ce_tb is
end entity;

architecture sim of proc_ce_tb is
  -- inout through a const function: 5 -> 6
  function k_bump return integer is
    variable v : integer := 5;
  begin
    bump(v);
    return v;
  end function;

  -- out mode + interface default (omitted actual) + nested chain + loop:
  -- 0 -> 12 -> 14 -> 24 -> 49 -> 59
  function k_chain return integer is
    variable v : integer := 0;
  begin
    triple(v, 4);
    step_by(v);
    step_by(v, 10);
    outer(v);
    sum_to(v, 4);
    return v;
  end function;

  -- Self-recursive: the fold aborts (W0401) and the constant is X.
  function k_spin return integer is
    variable v : integer := 0;
  begin
    spin(v);
    return v;
  end function;

  constant KB : integer := k_bump;
  constant KC : integer := k_chain;
  constant KS : integer := k_spin;
begin
  main : process
  begin
    assert KB = 6 report "ce_tb: KB wrong" severity error;
    assert KC = 59 report "ce_tb: KC wrong" severity error;
    report "ce_tb: KS=" & integer'image(KS);
    report "ce_tb: PASS";
    wait;
  end process;
end architecture;
