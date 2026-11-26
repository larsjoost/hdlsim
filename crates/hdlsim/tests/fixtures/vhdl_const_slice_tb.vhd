-- Array constants addressed by index and slice, ascending and descending
-- ranges, low bounds off zero, user array types, and runtime (signal-target)
-- reads. Element reads must follow the declared range direction, not the
-- flat value-bit order: element k of an ascending range is value element k,
-- of a descending range value element size-1-k.
package slice_pk is
  type nibbles_t is array (natural range <>) of bit;
end package;

use work.slice_pk.all;

entity const_slice_tb is
end entity;

architecture sim of const_slice_tb is
  constant ASC_DATA  : bit_vector(0 to 7) := "00010011";
  constant DESC_DATA : bit_vector(7 downto 0) := "00010011";
  constant HIGH_ASC  : bit_vector(8 to 15) := "00010011";
  constant OFFS_DESC : bit_vector(15 downto 8) := "00010011";
  constant NIB       : nibbles_t(0 to 7) := "00010011";
  constant VEC_DESC  : bit_vector(7 downto 0) := "00010011";
  -- Single elements: ascending element 3 is the 4th character, descending
  -- element 0 is the last character.
  constant ASC_3   : bit := ASC_DATA(3);
  constant DESC_0  : bit := DESC_DATA(0);
  constant HIGH_9  : bit := HIGH_ASC(9);
  constant NIB_6   : bit := NIB(6);
  constant OFFS_10 : bit := OFFS_DESC(10);
  constant VEC_0   : bit := VEC_DESC(0);
  -- Slices: bounds are element positions of the base's own range.
  constant ASC_LO   : bit_vector(0 to 3) := ASC_DATA(0 to 3);
  constant ASC_HI   : bit_vector(4 to 7) := ASC_DATA(4 to 7);
  constant HIGH_MID : bit_vector(10 to 13) := HIGH_ASC(10 to 13);
  constant NIB_MID  : nibbles_t(2 to 5) := NIB(2 to 5);
  constant OFFS_MID : bit_vector(12 downto 10) := OFFS_DESC(12 downto 10);
  signal OUTV : bit_vector(3 downto 0);
begin
  main : process
  begin
    assert ASC_3 = '1' report "slice_tb: ASC_DATA(3) wrong" severity error;
    assert DESC_0 = '1' report "slice_tb: DESC_DATA(0) wrong" severity error;
    assert HIGH_9 = '0' report "slice_tb: HIGH_ASC(9) wrong" severity error;
    assert NIB_6 = '1' report "slice_tb: NIB(6) wrong" severity error;
    assert OFFS_10 = '0' report "slice_tb: OFFS_DESC(10) wrong" severity error;
    assert VEC_0 = '1' report "slice_tb: VEC_DESC(0) wrong" severity error;
    assert ASC_LO = "0001" report "slice_tb: ASC_DATA(0 to 3) wrong" severity error;
    assert ASC_HI = "0011" report "slice_tb: ASC_DATA(4 to 7) wrong" severity error;
    assert HIGH_MID = "0100" report "slice_tb: HIGH_ASC(10 to 13) wrong" severity error;
    assert NIB_MID = "0100" report "slice_tb: NIB(2 to 5) wrong" severity error;
    assert OFFS_MID = "100" report "slice_tb: OFFS_DESC(12 downto 10) wrong" severity error;
    -- Runtime read of a constant slice with a signal target.
    OUTV <= OFFS_DESC(15 downto 12);
    wait for 1 ns;
    assert OUTV = "0001" report "slice_tb: runtime OFFS_DESC slice wrong" severity error;
    report "slice_tb: PASS";
    wait;
  end process;
end architecture;
