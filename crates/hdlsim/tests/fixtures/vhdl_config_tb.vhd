-- Self-checking testbench for VHDL configurations:
--  * `configuration cfg_top1 of top1` with a block configuration
--    (`for u2 : buf use entity work.buf(sim);`) — instantiating the
--    configuration directly (`dut : configuration work.cfg_top1`).
--  * a configuration specification in the architecture declarative part
--    (`for u2 : buf use entity work.buf(sim);`) on top2.
--  * `for all : g2 use entity work.g2(rtl) generic map (N => 4);` with a
--    generic map in the binding (cfg_top3).
library ieee;
use ieee.std_logic_1164.all;
use std.env.all;

entity inv is
  port (a : in std_ulogic; y : out std_ulogic);
end entity;

architecture rtl of inv is
begin
  y <= not a;
end architecture;

architecture sim of inv is
begin
  y <= a; -- deliberately wrong (buffer)
end architecture;

entity buf is
  port (a : in std_ulogic; y : out std_ulogic);
end entity;

architecture rtl of buf is
begin
  y <= a;
end architecture;

architecture sim of buf is
begin
  y <= not a; -- deliberately wrong (inverter)
end architecture;

entity g2 is
  generic (N : natural := 8);
  port (a : in std_ulogic; y : out std_ulogic_vector(N-1 downto 0));
end entity;

architecture rtl of g2 is
begin
  y <= (others => a);
end architecture;

entity top1 is
  port (i : in std_ulogic; o : out std_ulogic);
end entity;

architecture structural of top1 is
  signal mid : std_ulogic;
begin
  u1 : entity work.inv(rtl) port map (a => i, y => mid);
  u2 : entity work.buf port map (a => mid, y => o);
end architecture;

-- u2 bound to buf/sim (inverter): o = not(not(i)) = i.
configuration cfg_top1 of top1 is
  for structural
    for u2 : buf
      use entity work.buf(sim);
    end for;
  end for;
end configuration;

-- Default binding: buf/rtl stays the default for plain `entity work.buf`.
configuration cfg_top1_default of top1 is
  for structural
  end for;
end configuration;

entity top2 is
  port (i : in std_ulogic; o : out std_ulogic);
end entity;

architecture structural of top2 is
  -- configuration specification in the declarative part
  for u2 : buf use entity work.buf(sim);
  signal mid : std_ulogic;
begin
  u1 : entity work.inv(rtl) port map (a => i, y => mid);
  u2 : entity work.buf port map (a => mid, y => o);
end architecture;

entity top3 is
  port (i : in std_ulogic; o : out std_ulogic_vector(3 downto 0));
end entity;

architecture structural of top3 is
begin
  u : entity work.g2 port map (a => i, y => o);
end architecture;

-- `for all` with a generic map in the binding: N => 4.
configuration cfg_top3 of top3 is
  for structural
    for all : g2
      use entity work.g2(rtl) generic map (N => 4);
    end for;
  end for;
end configuration;

entity tb_config is
end entity;

architecture sim of tb_config is
  signal i : std_ulogic := '0';
  signal o1, o2 : std_ulogic;
  signal o3 : std_ulogic_vector(3 downto 0);
begin
  -- configuration instantiation with a block configuration
  dut1 : configuration work.cfg_top1 port map (i => i, o => o1);
  -- architecture with a configuration specification
  dut2 : entity work.top2(structural) port map (i => i, o => o2);
  -- configuration instantiation with `for all` + generic map
  dut3 : configuration work.cfg_top3 port map (i => i, o => o3);

  process
  begin
    i <= '0';
    wait for 1 ns;
    -- both configurations invert twice: o = i
    assert o1 = '0' report "cfg block binding failed (o1)" severity failure;
    assert o2 = '0' report "config spec binding failed (o2)" severity failure;
    assert o3 = "0000" report "for-all/generic-map binding failed (o3)" severity failure;
    i <= '1';
    wait for 1 ns;
    assert o1 = '1' report "cfg block binding failed (o1)" severity failure;
    assert o2 = '1' report "config spec binding failed (o2)" severity failure;
    assert o3 = "1111" report "for-all/generic-map binding failed (o3)" severity failure;
    report "PASS: vhdl configurations";
    std.env.stop;
  end process;
end architecture;
