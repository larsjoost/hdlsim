-- Regression: deep selected-name chains over records with an array index in
-- the middle (`outer.inner.defs(idx).info.tag`), both rooted at a
-- record-typed constant and at a record-typed function formal, with
-- `others` choices inside nested aggregates and an unconstrained-array
-- field whose element width comes from the field's decl constraint.
--
-- Failing shapes before the fixes: the chain walker stopped at the index
-- (fields after it unresolved or silently X), a record field typed as an
-- unconstrained array took the element width instead of the decl
-- constraint's, and aggregate `others` inside nested records lost the
-- field order.

package bloom_pkg is
  type petal_t is record
    tag  : integer;
    note : integer;
  end record;
  type petal_vec_t is array (natural range <>) of petal_t;
  type cluster_t is record
    petals : petal_vec_t(0 to 3);
  end record;
  type bloom_t is record
    cluster : cluster_t;
  end record;

  function petal_tag(b : bloom_t; idx : integer) return integer;
  function petal_note(b : bloom_t; idx : integer) return integer;
end package;

package body bloom_pkg is
  function petal_tag(b : bloom_t; idx : integer) return integer is
  begin
    return b.cluster.petals(idx).tag;
  end function;

  function petal_note(b : bloom_t; idx : integer) return integer is
  begin
    return b.cluster.petals(idx).note;
  end function;
end package;

use work.bloom_pkg.all;

entity deep_chain_tb is
end entity;

architecture sim of deep_chain_tb is
  -- Constant-rooted chain with an index in the middle and `others` in the
  -- nested aggregates.
  constant C_BLOOM : bloom_t :=
    (cluster =>
      (petals =>
        (0 => (tag => 7, note => 70), others => (tag => 0, note => 0))));
  constant C_MID_TAG  : integer := C_BLOOM.cluster.petals(0).tag;
  constant C_LEAF_TAG : integer := C_BLOOM.cluster.petals(1).tag;

  -- Function-rooted chain: same segments, root is a record-typed formal.
  constant C_FN_TAG  : integer := petal_tag(C_BLOOM, 0);
  constant C_FN_NOTE : integer := petal_note(C_BLOOM, 0);
begin
  main : process
  begin
    report "C_MID_TAG=" & integer'image(C_MID_TAG);
    report "C_LEAF_TAG=" & integer'image(C_LEAF_TAG);
    report "C_FN_TAG=" & integer'image(C_FN_TAG);
    report "C_FN_NOTE=" & integer'image(C_FN_NOTE);
    wait;
  end process;
end architecture;
