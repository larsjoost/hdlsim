library std;
use std.env.all;
use work.fnlocal_pkg.all;

entity tb_fnlocalconst is
end entity;

architecture sim of tb_fnlocalconst is
begin
  process
  begin
    assert rmsb_of(8) = 7
      report "rmsb_of(8) failed" severity error;
    assert rmsb_of(16) = 15
      report "rmsb_of(16) failed" severity error;
    report "PASS: fn-local const per call";
    std.env.finish;
    wait;
  end process;
end architecture;
