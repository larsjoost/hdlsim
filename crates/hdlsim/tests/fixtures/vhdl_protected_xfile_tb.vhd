-- Self-checking testbench in a separate file from the protected-type package:
-- `use work.prot_pkg.all;` must expose counter_t's methods, so calls dispatch
-- to the real t::m bodies instead of returning X.
use work.prot_pkg.all;

entity tb_protected_xfile is
end entity;

architecture sim of tb_protected_xfile is
  shared variable sv : counter_t;
begin
  process
  begin
    assert sv.get = 0 report "initial count wrong: " & integer'image(sv.get) severity error;
    sv.inc;
    sv.inc;
    sv.add(10);
    assert sv.get = 12 report "count wrong: " & integer'image(sv.get) severity error;
    report "PASS: vhdl protected types cross-file";
    wait;
  end process;
end architecture;
