-- Regression: string and bit_vector element/slice addressing on variables
-- and signals must address by the object's own range, not raw bit
-- positions:
--   * string variable: slice-assign, slice read, single-char read/write,
--     whole-variable assign,
--   * ascending bit_vector local: slice and index writes land on the
--     mirrored (MSB-first) element positions,
--   * descending and non-zero-low signals: slice writes clamp to the
--     recorded range.
-- The pre-fix lowerer addressed 1-based strings at bit offsets 0..n and
-- ascending vectors at raw element indexes, corrupting neighboring
-- characters/bits.
entity strvar_tb is
end entity;

architecture sim of strvar_tb is
  signal sa : bit_vector(0 to 7) := "00000000";
  signal sd : bit_vector(7 downto 0) := "00000000";
  signal nz : bit_vector(15 downto 8) := "00000000";
begin

  main : process
    variable s : string(1 to 5) := "abcde";
    variable t : string(1 to 5) := (others => ' ');
    variable c : character;
    variable bv : bit_vector(0 to 7) := "00000000";
    variable st : string(1 to 8) := "aaaaaaaa";
  begin
    -- String variable addressing.
    report "P0=[" & s & "]";
    report "P1=[" & t & "]";
    s(2 to 4) := "xyz";
    report "P2=[" & s & "]";
    t(1 to 3) := "abc";
    report "P3=[" & t & "]";
    report "P4=[" & s(2 to 4) & "]";
    c := s(5);
    report "P5=[" & c & "]";
    s(1) := 'q';
    report "P6=[" & s & "]";
    t := "hello";
    report "P7=[" & t & "]";
    -- Ascending bit_vector local.
    bv(0 to 2) := "111";
    report "BVS=[" & to_bstring(bv) & "]";
    bv(7) := '1';
    report "BVI=[" & to_bstring(bv) & "]";
    st(1 to 3) := "xyz";
    report "STS=[" & st & "]";
    -- Signal slice writes across range shapes.
    sa(0 to 2) <= "111";
    sd(7 downto 5) <= "111";
    nz(15 downto 13) <= "111";
    wait for 1 ns;
    report "SA=[" & to_bstring(sa) & "]";
    report "SD=[" & to_bstring(sd) & "]";
    report "NZ=[" & to_bstring(nz) & "]";
    wait;
  end process;

end architecture;
