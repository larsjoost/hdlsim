-- Self-checking testbench for VHDL-2008 protected types: state variables in
-- the protected body, method bodies mutating that state, and calls on a
-- shared-variable object. Previously methods were stubs returning X.
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

use work.prot_pkg.all;

entity tb_protected is
end entity;

architecture sim of tb_protected is
  shared variable sv : counter_t;
begin
  process
  begin
    assert sv.get = 0 report "initial count wrong: " & integer'image(sv.get) severity error;
    sv.inc;
    sv.inc;
    sv.add(10);
    assert sv.get = 12 report "count wrong: " & integer'image(sv.get) severity error;
    report "PASS: vhdl protected types";
    wait;
  end process;
end architecture;
