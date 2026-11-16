-- Self-checking testbench for architecture selection: `entity work.dual(rtl)`
-- picks the `rtl` architecture, and a `configuration … for rtl` makes `rtl`
-- the default binding for plain `entity work.dual2` instantiations.
-- (Previously all architectures merged onto one module and the `(arch)`
-- selector was dropped, so the last architecture always won.)
library ieee;
use ieee.std_logic_1164.all;
use std.env.all;

entity dual is
  port (i : in std_ulogic; o : out std_ulogic);
end entity;

architecture rtl of dual is
begin
  o <= i;
end architecture;

architecture sim of dual is
begin
  o <= not i;
end architecture;

entity dual2 is
  port (i : in std_ulogic; o : out std_ulogic);
end entity;

architecture rtl of dual2 is
begin
  o <= i;
end architecture;

architecture sim of dual2 is
begin
  o <= not i;
end architecture;

configuration cfg_dual2 of dual2 is
  for rtl
  end for;
end configuration;

entity tb_arch_select is
end entity;

architecture sim of tb_arch_select is
  signal i1, o1 : std_ulogic := '0';
  signal i2, o2 : std_ulogic := '0';
begin
  u_rtl : entity work.dual(rtl) port map (i => i1, o => o1);
  u_def : entity work.dual2 port map (i => i2, o => o2);

  process
  begin
    i1 <= '1';
    i2 <= '1';
    wait for 1 ns;
    assert o1 = '1' report "arch (rtl): o1 should be 1" severity error;
    assert o2 = '1' report "default arch via configuration: o2 should be 1" severity error;
    report "PASS: vhdl arch select";
    std.env.finish;
    wait;
  end process;
end architecture;
