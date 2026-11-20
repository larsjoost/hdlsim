-- Protected type declared in its own file: using files must see the state
-- fields and methods (exported via PackageIr::prot_tys), not placeholders.
package prot_pkg is
  type counter_t is protected
    procedure inc;
    procedure add(n : integer);
    impure function get return integer;
  end protected;
end package;

package body prot_pkg is
  type counter_t is protected body
    variable count : integer := 0;
    procedure inc is
    begin
      count := count + 1;
    end procedure;
    procedure add(n : integer) is
    begin
      count := count + n;
    end procedure;
    impure function get return integer is
    begin
      return count;
    end function;
  end protected body;
end package body;
