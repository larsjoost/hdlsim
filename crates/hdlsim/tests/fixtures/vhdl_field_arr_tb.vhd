-- Regression: a slice-assign whose LHS is a record field that is itself an
-- array (`result.options(0 to n)`) must remap the element bounds inside the
-- field's packed value — the pre-fix lowerer emitted raw element indexes as
-- bit positions, silently corrupting (or X-ing) every folded constant that
-- built a record through a field-array slice-assign. Checksums return
-- integers so the assertion avoids the record-constant display path.
package farr_pkg is
  type opt_t is record
    ident : string(1 to 4);
    mask  : string(1 to 4);
  end record;
  type opt_array_t is array (natural range <>) of opt_t;
  type info_t is record
    options : opt_array_t(0 to 3);
    count   : integer;
  end record;
  constant BLANK_OPT : opt_t := (ident => (others => ' '), mask => (others => '.'));
  function set_opts(opts : opt_array_t) return integer;
end package;

package body farr_pkg is
  function set_opts(opts : opt_array_t) return integer is
    variable result : info_t;
    variable sum : integer := 0;
  begin
    result.options(0 to opts'length - 1) := opts;
    result.count := opts'length;
    if result.options(0).ident = "ab  " then
      sum := sum + 1;
    end if;
    if result.options(1).ident = "cd  " then
      sum := sum + 10;
    end if;
    if result.options(2).ident = "    " then
      sum := sum + 100;
    end if;
    if result.count = 2 then
      sum := sum + 1000;
    end if;
    return sum;
  end function;
end package body;

entity farr_tb is
end entity;

architecture sim of farr_tb is
begin
  main : process
    constant S : integer := farr_pkg.set_opts(
      (farr_pkg.BLANK_OPT, farr_pkg.BLANK_OPT));
    constant A : opt_array_t(0 to 1) := (
      0 => (ident => "ab  ", mask => "...."),
      1 => (ident => "cd  ", mask => "...."));
    constant S2 : integer := farr_pkg.set_opts(A);
  begin
    report "S=" & integer'image(S);
    report "S2=" & integer'image(S2);
    wait;
  end process;
end architecture;
