-- Procedure `variable inout`/`out` copy-back: the caller's variable
-- receives the formal's final value on return (LRM 1076 §4.3) — at process
-- level (where process variables are flat signals) and through nested
-- procedure frames alike, for scalars and array-element actuals.
package proc_pk is
  procedure bump(variable x : inout integer);
  procedure double(variable x : inout integer);
  procedure clamp_max(variable x : out integer; lo : in integer);
  procedure or_in(variable acc : inout bit; x : in bit);
  procedure setb(variable v : inout bit);
  procedure outer(variable n : inout integer);
end package;

package body proc_pk is
  procedure bump(variable x : inout integer) is
  begin
    x := x + 1;
  end procedure;

  procedure double(variable x : inout integer) is
  begin
    x := x * 2;
  end procedure;

  procedure clamp_max(variable x : out integer; lo : in integer) is
  begin
    x := lo + 3;
  end procedure;

  procedure or_in(variable acc : inout bit; x : in bit) is
  begin
    if x = '1' then
      acc := '1';
    end if;
  end procedure;

  procedure setb(variable v : inout bit) is
  begin
    v := '1';
  end procedure;

  procedure inner(variable n : inout integer) is
  begin
    n := n * 2;
  end procedure;

  -- Nested inout chain: the inner call's copy-back must land in this
  -- frame's slot, and this procedure's in the process's variable.
  procedure outer(variable n : inout integer) is
  begin
    inner(n);
    n := n + 1;
  end procedure;
end package body;

use work.proc_pk.all;

entity proc_inout_tb is
end entity;

architecture sim of proc_inout_tb is
  type bits_t is array (0 to 2) of bit;
begin
  main : process
    variable n   : integer := 5;
    variable m   : integer := 3;
    variable o   : integer := 0;
    variable n2  : integer := 3;
    variable acc : bit := '0';
    variable v   : bits_t := ('0', '0', '0');
  begin
    bump(n);
    assert n = 6 report "inout_tb: bump wrong" severity error;
    double(m);
    assert m = 6 report "inout_tb: double wrong" severity error;
    clamp_max(o, 4);
    assert o = 7 report "inout_tb: out mode wrong" severity error;
    outer(n2);
    assert n2 = 7 report "inout_tb: nested chain wrong" severity error;
    or_in(acc, '1');
    assert acc = '1' report "inout_tb: bit inout wrong" severity error;
    setb(v(1));
    assert v(1) = '1' report "inout_tb: element actual wrong" severity error;
    assert v(0) = '0' report "inout_tb: sibling element disturbed" severity error;
    report "inout_tb: PASS";
    wait;
  end process;
end architecture;
