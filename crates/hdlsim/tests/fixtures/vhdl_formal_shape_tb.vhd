-- Shape-at-binding: an unconstrained 1-D vector formal takes the actual's
-- index range (LRM 1076 §5.2.1). Element and slice writes and reads through
-- the formal must land at the actual's layout — ascending and descending
-- actuals alike — at runtime and inside const-eval'd constant initializers,
-- including procedure chains where the actual is itself a shape-bound
-- formal, and mirroring the same operation done directly on a local.
package shape_pk is
  procedure set_elem(variable arr : inout bit_vector; i : in integer; x : in bit);
  procedure set_slice(variable arr : inout bit_vector; hi : in integer; lo : in integer; x : in bit);
  procedure chain_elem(variable arr : inout bit_vector; i : in integer; x : in bit);
  function get_elem(arr : bit_vector; i : in integer) return bit;
  function build_up return bit_vector;
  function build_dn return bit_vector;
  function build_slice return bit_vector;
  function build_chain return bit_vector;
end package;

package body shape_pk is
  procedure set_elem(variable arr : inout bit_vector; i : in integer; x : in bit) is
  begin
    arr(i) := x;
  end procedure;

  procedure set_slice(variable arr : inout bit_vector; hi : in integer; lo : in integer; x : in bit) is
  begin
    arr(hi downto lo) := (others => x);
  end procedure;

  -- The actual is itself a shape-bound formal: the inner call's bind must
  -- chain to the outer frame's binding rather than fall back to raw
  -- addressing.
  procedure chain_elem(variable arr : inout bit_vector; i : in integer; x : in bit) is
  begin
    set_elem(arr, i, x);
  end procedure;

  function get_elem(arr : bit_vector; i : in integer) return bit is
  begin
    return arr(i);
  end function;

  function build_up return bit_vector is
    variable r : bit_vector(0 to 3) := "0000";
  begin
    set_elem(r, 2, '1');
    return r;
  end function;

  function build_dn return bit_vector is
    variable r : bit_vector(3 downto 0) := "0000";
  begin
    set_elem(r, 1, '1');
    return r;
  end function;

  function build_slice return bit_vector is
    variable r : bit_vector(0 to 3) := "0000";
  begin
    set_slice(r, 2, 1, '1');
    return r;
  end function;

  function build_chain return bit_vector is
    variable r : bit_vector(0 to 3) := "0000";
  begin
    chain_elem(r, 2, '1');
    return r;
  end function;
end package body;

use work.shape_pk.all;

entity formal_shape_tb is
end entity;

architecture sim of formal_shape_tb is
  constant BUILT_UP : bit_vector(0 to 3) := build_up;
  constant BUILT_DN : bit_vector(3 downto 0) := build_dn;
  constant BUILT_SL : bit_vector(0 to 3) := build_slice;
  constant BUILT_CH : bit_vector(0 to 3) := build_chain;
  constant PAT : bit_vector(0 to 3) := "0010";
  constant RD : bit := get_elem(PAT, 2);
begin
  main : process
    variable up    : bit_vector(0 to 3) := "0000";
    variable dn    : bit_vector(3 downto 0) := "0000";
    variable sl_up : bit_vector(0 to 3) := "0000";
    variable sl_dn : bit_vector(3 downto 0) := "0000";
    variable mir   : bit_vector(0 to 3) := "0000";
  begin
    -- Element write through an unconstrained formal, ascending actual.
    set_elem(up, 2, '1');
    assert up(2) = '1' report "formal_shape_tb: up(2) wrong" severity error;
    assert up(0) = '0' report "formal_shape_tb: up(0) wrong" severity error;
    assert up(1) = '0' report "formal_shape_tb: up(1) wrong" severity error;
    assert up(3) = '0' report "formal_shape_tb: up(3) wrong" severity error;
    -- Mirroring: the same operation on a local (static remap) must leave
    -- the identical value.
    mir(2) := '1';
    assert up = mir report "formal_shape_tb: formal vs direct mirror wrong" severity error;
    -- Element write, descending actual.
    set_elem(dn, 1, '1');
    assert dn(1) = '1' report "formal_shape_tb: dn(1) wrong" severity error;
    assert dn(0) = '0' report "formal_shape_tb: dn(0) wrong" severity error;
    assert dn(2) = '0' report "formal_shape_tb: dn(2) wrong" severity error;
    assert dn(3) = '0' report "formal_shape_tb: dn(3) wrong" severity error;
    -- Slice writes through a formal, both actual directions.
    set_slice(sl_up, 2, 1, '1');
    assert sl_up(1) = '1' report "formal_shape_tb: sl_up(1) wrong" severity error;
    assert sl_up(2) = '1' report "formal_shape_tb: sl_up(2) wrong" severity error;
    assert sl_up(0) = '0' report "formal_shape_tb: sl_up(0) wrong" severity error;
    assert sl_up(3) = '0' report "formal_shape_tb: sl_up(3) wrong" severity error;
    set_slice(sl_dn, 2, 1, '1');
    assert sl_dn(1) = '1' report "formal_shape_tb: sl_dn(1) wrong" severity error;
    assert sl_dn(2) = '1' report "formal_shape_tb: sl_dn(2) wrong" severity error;
    assert sl_dn(0) = '0' report "formal_shape_tb: sl_dn(0) wrong" severity error;
    assert sl_dn(3) = '0' report "formal_shape_tb: sl_dn(3) wrong" severity error;
    -- Element read through an unconstrained formal at runtime, and a
    -- chained procedure call whose actual is this frame's own variable.
    assert get_elem(up, 2) = '1' report "formal_shape_tb: get_elem wrong" severity error;
    chain_elem(up, 0, '1');
    assert up(0) = '1' report "formal_shape_tb: chain runtime wrong" severity error;
    assert up(2) = '1' report "formal_shape_tb: chain kept elem wrong" severity error;
    -- Const-eval: element and slice writes through shape-bound formals in
    -- constant initializers, ascending and descending, plus a chained
    -- outer-formal actual, and a read whose actual is a constant.
    assert BUILT_UP(2) = '1' report "formal_shape_tb: BUILT_UP(2) wrong" severity error;
    assert BUILT_UP(0) = '0' and BUILT_UP(1) = '0' and BUILT_UP(3) = '0' report "formal_shape_tb: BUILT_UP siblings wrong" severity error;
    assert BUILT_UP = "0010" report "formal_shape_tb: BUILT_UP literal eq wrong" severity error;
    assert BUILT_DN(1) = '1' report "formal_shape_tb: BUILT_DN(1) wrong" severity error;
    assert BUILT_DN(0) = '0' and BUILT_DN(2) = '0' and BUILT_DN(3) = '0' report "formal_shape_tb: BUILT_DN siblings wrong" severity error;
    assert BUILT_SL(1) = '1' and BUILT_SL(2) = '1' report "formal_shape_tb: BUILT_SL elems wrong" severity error;
    assert BUILT_SL(0) = '0' and BUILT_SL(3) = '0' report "formal_shape_tb: BUILT_SL siblings wrong" severity error;
    assert BUILT_SL = "0110" report "formal_shape_tb: BUILT_SL literal eq wrong" severity error;
    assert BUILT_CH(2) = '1' and BUILT_CH(0) = '0' and BUILT_CH(1) = '0' and BUILT_CH(3) = '0' report "formal_shape_tb: BUILT_CH wrong" severity error;
    assert RD = '1' report "formal_shape_tb: const-actual read wrong" severity error;
    report "formal_shape_tb: PASS";
    wait;
  end process;
end architecture;
