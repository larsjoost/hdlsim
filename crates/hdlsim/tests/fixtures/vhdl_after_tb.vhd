-- Self-checking testbench for sequential signal assignments with `after`
-- delays (`s <= v after t;`). Previously the delay was silently dropped (the
-- delay expression itself was assigned as the value), and a correct
-- implementation must not suspend the process.
library ieee;
use ieee.std_logic_1164.all;
use std.env.all;

entity tb_after is
end entity;

architecture sim of tb_after is
  signal s : std_logic := '0';
  signal q : std_logic := '0';
  signal r : std_logic := '0';
  signal c : std_logic := '0';
begin
  process
    variable v : integer := 0;
  begin
    -- delayed assignment: lands at t = 10 ns, process not suspended
    s <= '1' after 10 ns;
    v := 1;
    wait for 5 ns;
    assert s = '0' report "s must still be 0 at 5 ns, got " & to_string(s) severity error;
    assert v = 1 report "process must not block on `after`" severity error;
    wait for 10 ns; -- t = 15 ns
    assert s = '1' report "s must be 1 at 15 ns, got " & to_string(s) severity error;

    -- multiple waveform elements: each `after` relative to execution (t = 15 ns)
    q <= '1' after 5 ns, '0' after 10 ns;
    wait for 7 ns; -- t = 22 ns
    assert q = '1' report "q must be 1 at 22 ns, got " & to_string(q) severity error;
    wait for 5 ns; -- t = 27 ns
    assert q = '0' report "q must be 0 at 27 ns, got " & to_string(q) severity error;

    -- conditional delayed assignment: false condition schedules nothing
    r <= '1' after 10 ns when c = '1';
    wait for 15 ns; -- t = 42 ns
    assert r = '0' report "r must stay 0 (condition false), got " & to_string(r) severity error;

    c <= '1';
    wait for 1 ns; -- t = 43 ns
    r <= '1' after 10 ns when c = '1';
    wait for 15 ns; -- t = 58 ns
    assert r = '1' report "r must be 1 at 58 ns, got " & to_string(r) severity error;

    report "PASS: vhdl after-delay";
    std.env.finish;
  end process;
end architecture;
