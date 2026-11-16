-- Self-checking testbench for for/if/case-generate and block statements:
-- generate bodies are unrolled at lower time (loop variable as a constant),
-- if/case-generate lower the first matching branch, blocks lower their
-- declarations and statements into the enclosing architecture, and each
-- for-generate iteration gets its own copy of the declarative-part signals.
library ieee;
use ieee.std_logic_1164.all;
use std.env.all;

entity inv1 is
  port (x : in std_ulogic; y : out std_ulogic);
end entity;

architecture rtl of inv1 is
begin
  y <= not x;
end architecture;

entity tb_generate is
  generic (W : integer := 4);
end entity;

architecture sim of tb_generate is
  signal a : std_ulogic_vector(W-1 downto 0) := (others => '0');
  signal b : std_ulogic_vector(W-1 downto 0);
  signal c : std_ulogic_vector(W-1 downto 0);
  signal d, e, f, g, h : std_ulogic;
  signal acc : std_ulogic_vector(W-1 downto 0);
begin
  -- for-generate, ascending, with a per-iteration signal and an instance
  gen_b: for i in 0 to W-1 generate
    signal xi, yi : std_ulogic;
  begin
    xi <= a(i);
    u: entity work.inv1 port map (x => xi, y => yi);
    b(i) <= yi;
  end generate;

  -- for-generate, descending
  gen_c: for i in W-1 downto 0 generate
    c(i) <= a(i);
  end generate;

  -- if-generate with elsif / else chain
  gen_d: if W = 8 generate
    d <= '1';
  elsif W = 4 generate
    d <= '0';
  else generate
    d <= '1';
  end if generate;

  gen_e: if W = 99 generate
    e <= '1';
  else generate
    e <= '0';
  end if generate;

  -- case-generate: matching `when`
  gen_f: case W generate
    when 4 =>
      f <= '1';
    when others =>
      f <= '0';
  end case generate;

  -- case-generate: `when others`
  gen_g: case W generate
    when 8 =>
      g <= '1';
    when others =>
      g <= '0';
  end case generate;

  -- block with a local signal
  blk: block
    signal t : std_ulogic;
  begin
    t <= '1';
    h <= a(0) and t;
  end block;

  -- nested if-generate inside for-generate
  gen_acc: for i in 0 to W-1 generate
    inner: if i < 2 generate
      acc(i) <= a(i);
    else generate
      acc(i) <= '0';
    end generate;
  end generate;

  process
  begin
    a <= x"5";
    wait for 1 ns;
    assert b = x"A" report "for-generate + instance failed" severity error;
    assert c = x"5" report "for-generate downto failed" severity error;
    assert d = '0' report "if-generate elsif failed" severity error;
    assert e = '0' report "if-generate else failed" severity error;
    assert f = '1' report "case-generate match failed" severity error;
    assert g = '0' report "case-generate others failed" severity error;
    assert h = '1' report "block failed" severity error;
    assert acc = x"1" report "nested generate failed" severity error;
    report "PASS: vhdl generate/block";
    std.env.finish;
    wait;
  end process;
end architecture;
