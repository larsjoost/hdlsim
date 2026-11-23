-- Fixture: constant initializers that call package functions which in turn
-- call package-internal siblings. The testbench merges only one function per
-- use clause (named visibility), so the sibling calls must resolve through
-- the lowerer's package-wide const-eval fallback.
package ce_fallback_pkg is
  constant C_IDLE_DELAY : natural := 3;

  function get_leaf_delay return natural;
  function get_stage_delay (constant deep : boolean) return natural;
  function get_chain_delay return natural;
  function get_first_stop_delay (constant limit : natural) return natural;
  function get_odd_count_delay (constant limit : natural) return natural;
  function get_even_count_delay (constant limit : natural) return natural;
end package;

package body ce_fallback_pkg is
  function get_leaf_delay return natural is
  begin
    return 5;
  end function;

  function get_stage_delay (
    constant deep : boolean)
    return natural is
  begin
    if (deep) then
      return get_leaf_delay;
    end if;
    return C_IDLE_DELAY;
  end function;

  function get_chain_delay return natural is
  begin
    return get_stage_delay(deep => true) + 1;
  end function;

  -- Loop with an unconditional exit: const-eval must consume the exit and
  -- keep the value accumulated before it.
  function get_first_stop_delay (
    constant limit : natural)
    return natural is
    variable acc_v : natural := 0;
  begin
    for i in 0 to limit - 1 loop
      if (i = 3) then
        acc_v := acc_v + 1;
        exit;
      end if;
      acc_v := acc_v + 2;
    end loop;
    return acc_v;
  end function;

  -- Loop with an unconditional next: the iteration is skipped but the loop
  -- keeps running (const-eval must not treat next as an exit).
  function get_odd_count_delay (
    constant limit : natural)
    return natural is
    variable acc_v : natural := 0;
  begin
    for i in 0 to limit - 1 loop
      if (i mod 2) = 0 then
        next;
      end if;
      acc_v := acc_v + 1;
    end loop;
    return acc_v;
  end function;

  -- Loop with a conditional next (`next when c;`): same skip, guarded.
  function get_even_count_delay (
    constant limit : natural)
    return natural is
    variable acc_v : natural := 0;
  begin
    for i in 0 to limit - 1 loop
      next when (i mod 2) = 1;
      acc_v := acc_v + 1;
    end loop;
    return acc_v;
  end function;
end package body;
