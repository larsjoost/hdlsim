-- Regression fixture: a process whose sensitivity list entry is an indexed
-- name (`data(i)` inside a generate loop). The entry must resolve to its
-- longest static prefix (the base signal): the parser wraps `data(i)` in a
-- Call node, and a lowerer that only accepts bare Name nodes silently drops
-- the entry. The process then runs with an empty sensitivity list, which the
-- kernel treats as a bare `always` — an infinite zero-delay re-run.
library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;

entity sens_idx_tb is
end entity;

architecture sim of sens_idx_tb is
  type arr_t is array (0 to 2) of signed(7 downto 0);
  signal data : arr_t := (others => (others => '0'));
  signal copy : arr_t := (others => (others => '0'));
begin

  gen_blk : FOR i IN 0 TO 2 GENERATE
    PROCESS (data(i))
    BEGIN
      copy(i) <= data(i);
    END PROCESS;
  END GENERATE gen_blk;

  stim : PROCESS
  BEGIN
    data(0) <= x"01";
    data(1) <= x"02";
    data(2) <= x"03";
    wait for 20 ns;
    assert copy(0) = to_signed(1, 8) report "copy(0) mismatch" severity error;
    assert copy(1) = to_signed(2, 8) report "copy(1) mismatch" severity error;
    assert copy(2) = to_signed(3, 8) report "copy(2) mismatch" severity error;
    report "SENSIDX done";
    wait;
  END PROCESS;

end architecture;
