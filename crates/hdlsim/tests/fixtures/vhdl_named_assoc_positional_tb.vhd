-- Positional generic map associations bind to the entity's generics in
-- declaration order. Regression guard: they used to leak into the port
-- connections (shifting them) when the => arrow lexed as two punctuation
-- tokens, and were later rejected outright instead of bound.
library ieee;
use ieee.std_logic_1164.all;

entity na_pos_dut is
  generic (
    G_W   : integer := 4;
    G_INV : boolean := false);
  port (
    d : in  std_logic_vector(G_W-1 downto 0);
    q : out std_logic_vector(G_W-1 downto 0));
end entity;

architecture rtl of na_pos_dut is
begin
  q <= not d when G_INV else d;
end architecture;

entity tb_positional_generic is
end entity;

architecture sim of tb_positional_generic is
  signal d1 : std_logic_vector(7 downto 0) := x"3C";
  signal q1 : std_logic_vector(7 downto 0);
  signal d2 : std_logic_vector(3 downto 0) := "0101";
  signal q2 : std_logic_vector(3 downto 0);
begin
  -- Positional: G_W=8, G_INV=true → q1 = not d1 = x"C3".
  u1 : entity work.na_pos_dut
    generic map (8, true)
    port map (d => d1, q => q1);
  -- Named, for contrast: G_W=4 (default), G_INV=false → q2 = d2.
  u2 : entity work.na_pos_dut
    generic map (G_W => 4, G_INV => false)
    port map (d => d2, q => q2);

  check : process
  begin
    wait for 1 ns;
    if q1 = x"C3" and q2 = "0101" then
      report "PASS: positional and named generic maps bind correctly";
    else
      report "FAIL: generic map binding" severity error;
    end if;
    wait;
  end process;
end architecture;
