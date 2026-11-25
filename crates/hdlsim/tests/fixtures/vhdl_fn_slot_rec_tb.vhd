-- Regression: record-typed objects keep their record mapping while functions
-- are lowered. Function formals occupy the same flat signal-id space as
-- module signals (port/slot ids overlap), so the per-function slot setup
-- must not erase a record signal's entry, and the entry must be reset to
-- its pre-call state after the body — a later function's integer formal
-- must not inherit the earlier function's record type.
--
-- Failing shape before the fix: the entity's record-typed signal (slot id
-- below the function's slot count) lost its record mapping when a
-- package-body function with more formals than the signal's id was
-- lowered; every later `sig.field` select then failed with
-- `error[E0401]: unknown identifier '<field>'`. The nested-procedure case
-- covers the same overlap for outer-scope formals: a nested procedure must
-- still see the enclosing procedure's record-typed formal.

package fn_slot_pkg is
  type cell_t is record
    tag : integer;
  end record;
  type cell_vec_t is array (natural range <>) of cell_t;
  type bank_t is record
    cells : cell_vec_t(0 to 3);
  end record;

  function pick(cells : cell_vec_t; idx : integer) return integer;
  function mix(a : integer; b : integer; c : integer) return integer;
end package;

package body fn_slot_pkg is
  function pick(cells : cell_vec_t; idx : integer) return integer is
  begin
    -- Record-typed formal: the slot setup maps slot ids to the record.
    return cells(idx).tag;
  end function;

  function mix(a : integer; b : integer; c : integer) return integer is
  begin
    -- Three scalar formals occupy the same slot ids the entity's record
    -- signal holds — the slot reset must not carry a stale record here.
    return a + b * c;
  end function;
end package;

library ieee;
use ieee.std_logic_1164.all;
use work.fn_slot_pkg.all;

entity fn_slot_tb is
end entity;

architecture sim of fn_slot_tb is
  -- Declared before the functions below are lowered: its flat signal id
  -- collides with the functions' formal slot ids.
  signal bank_s : bank_t := (cells => (others => (tag => 5)));
  signal tag_o  : integer;
  signal flat_o : integer;
begin
  -- Concurrent use of the record field select AFTER the package-body
  -- functions were lowered: must still resolve the field.
  tag_o <= bank_s.cells(0).tag;

  -- Scalar formals must not inherit a record mapping from any earlier
  -- function with overlapping slot ids.
  flat_o <= mix(1, 2, pick(bank_s.cells, 1));

  main : process
  begin
    wait for 1 ns;
    report "TAG=" & integer'image(tag_o);
    report "PICK=" & integer'image(flat_o - 2); -- mix(1,2,5) = 11 -> 9
    wait;
  end process;
end architecture;
