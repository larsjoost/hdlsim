-- Regression: string-building functions must const-eval to exact ASCII.
-- The pre-fix lowerer let earlier subprograms' shape entries (recorded
-- under the same value-slot ids) bake into the slice bounds — the fold
-- aborted with W0401, or worse, folded silently corrupted bytes. It must
-- pin:
--   * a dynamic-width local with `(others => ' ')` init and a dynamic
--     `result(1 to arg'length)` slice-assign (the pad shape),
--   * an empty-string actual: the formal's `'length` is 0 and the guarded
--     assign must not run,
--   * a slice READ of an unconstrained string formal addressing 1-based
--     (`arg(1 to 4)` = chars 1..4, not 2..5),
--   * same-name overloads dispatching by arity,
--   * a package body replacing its spec stub.
package strpad_pkg is
  type row_t is array (natural range <>) of integer;

  -- Lowered first: an array formal records element shapes under the value
  -- slots the string functions below reuse.
  function total(rows : row_t) return integer;

  function pad(arg : string; constant size : integer) return string;
  function rep(arg : string) return string;
  function rep(arg : string; constant size : integer) return string;
  function head4(arg : string) return string;
end package;

package body strpad_pkg is
  function total(rows : row_t) return integer is
    variable sum : integer := 0;
  begin
    for i in rows'range loop
      sum := sum + rows(i);
    end loop;
    return sum;
  end function;

  function pad(arg : string; constant size : integer) return string is
    variable result : string(1 to size) := (others => ' ');
  begin
    if arg'length > 0 then
      result(1 to arg'length) := arg;
    end if;
    return result;
  end function;

  function rep(arg : string) return string is
    variable result : string(1 to 4) := (others => '.');
  begin
    if arg'length > 0 then
      result(1 to arg'length) := arg;
    end if;
    return result;
  end function;

  function rep(arg : string; constant size : integer) return string is
    variable result : string(1 to size) := (others => '.');
  begin
    if arg'length > 0 then
      result(1 to arg'length) := arg;
    end if;
    return result;
  end function;

  function head4(arg : string) return string is
    variable result : string(1 to 4) := (others => '.');
  begin
    if arg'length >= 4 then
      result := arg(1 to 4);
    elsif arg'length > 0 then
      result(1 to arg'length) := arg;
    end if;
    return result;
  end function;
end package body;

entity strpad_tb is
end entity;

architecture sim of strpad_tb is
begin
  main : process
    constant T3 : integer := strpad_pkg.total((1, 2, 3));
    constant P5 : string(1 to 5) := strpad_pkg.pad("abc", 5);
    constant P3 : string(1 to 3) := strpad_pkg.pad("hi", 3);
    constant P0 : string(1 to 4) := strpad_pkg.pad("", 4);
    constant R1 : string(1 to 4) := strpad_pkg.rep("ab");
    constant R2 : string(1 to 5) := strpad_pkg.rep("ab", 5);
    constant H6 : string(1 to 4) := strpad_pkg.head4("abcdef");
    constant H2 : string(1 to 4) := strpad_pkg.head4("hi");
  begin
    report "T3=" & integer'image(T3);
    report "P5=[" & P5 & "]";
    report "P3=[" & P3 & "]";
    report "P0=[" & P0 & "]";
    report "R1=[" & R1 & "]";
    report "R2=[" & R2 & "]";
    report "H6=[" & H6 & "]";
    report "H2=[" & H2 & "]";
    wait;
  end process;
end architecture;
