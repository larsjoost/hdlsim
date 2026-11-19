library std;
use std.env.all;
use work.fnlocal_pkg.all;

entity tb_fnlocalconst is
end entity;

architecture sim of tb_fnlocalconst is
  -- elaboration-level constant initialized by a loop-built function call
  -- (the remaining W0401 from the issue #106 report): const-evaluated now.
  constant c_ones : bit_vector := ones_vec(3);
begin
  process
  begin
    assert c_ones = "111"
      report "c_ones failed" severity error;
    assert rmsb_of(8) = 7
      report "rmsb_of(8) failed" severity error;
    assert rmsb_of(16) = 15
      report "rmsb_of(16) failed" severity error;
    -- variable initializer runs per call
    assert twice(4) = 8
      report "twice(4) failed" severity error;
    -- loop-built vector return, both const-folded and called at runtime
    assert ones_vec(3) = "111"
      report "ones_vec(3) failed" severity error;
    report "PASS: fn-local const per call";
    std.env.finish;
    wait;
  end process;
end architecture;
