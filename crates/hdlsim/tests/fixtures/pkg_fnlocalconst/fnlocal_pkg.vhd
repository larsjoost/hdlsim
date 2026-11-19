-- Function-local constants whose initializer depends on a parameter (the
-- std_logic_arith pattern from issue #106): they are evaluated on every
-- call, not const-folded to X at lower time. Also covers function-local
-- variables with initializers and loop-built vector returns.
package fnlocal_pkg is
  function rmsb_of(size : integer) return integer;
  function twice(size : integer) return integer;
  function ones_vec(n : integer) return bit_vector;
end package;

package body fnlocal_pkg is
  function rmsb_of(size : integer) return integer is
    constant rmsb : integer := size - 1;
  begin
    return rmsb;
  end function;

  function twice(size : integer) return integer is
    variable v : integer := size * 2;
  begin
    return v;
  end function;

  function ones_vec(n : integer) return bit_vector is
    variable v : bit_vector(0 to n - 1);
  begin
    for i in 0 to n - 1 loop
      v(i) := '1';
    end loop;
    return v;
  end function;
end package body;
