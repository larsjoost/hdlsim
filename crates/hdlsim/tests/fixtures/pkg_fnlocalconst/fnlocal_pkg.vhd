-- Function-local constants whose initializer depends on a parameter (the
-- std_logic_arith pattern from issue #106): they are evaluated on every
-- call, not const-folded to X at lower time.
package fnlocal_pkg is
  function rmsb_of(size : integer) return integer;
end package;

package body fnlocal_pkg is
  function rmsb_of(size : integer) return integer is
    constant rmsb : integer := size - 1;
  begin
    return rmsb;
  end function;
end package body;
