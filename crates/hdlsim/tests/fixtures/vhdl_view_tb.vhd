-- Regression fixture for VHDL-2019 views + record aliases + entity
-- declarative items visible in the architecture: a `view` declaration, a
-- view-mode port, a `V'converse` view alias, an external-name alias, and an
-- entity-declared constant used by the architecture (LRM 1076 §1.2/§13.3).
library ieee;
use ieee.std_logic_1164.all;

package view_pkg is
  type sub_t is record
    addr : std_logic_vector(7 downto 0);
    data : std_logic_vector(31 downto 0);
  end record;
  type rec_t is record
    wa : sub_t;
    wd : sub_t;
  end record;
  view subv of sub_t is
    addr : out;
    data : in;
  end view subv;
  view bus_view of rec_t is
    wa : view subv;
    wd : view subv;
  end view bus_view;
  alias bus_view_conv is bus_view'converse;
end package;

entity dut is
  generic (
    WIDTH : integer := 8
  );
  port (
    bus : view work.view_pkg.bus_view of work.view_pkg.rec_t;
    clk : in std_logic
  );
  -- Entity-declarative constant: visible in the architecture.
  constant ADDR_W : integer := WIDTH;
end entity;

architecture rtl of dut is
  signal defaults : work.view_pkg.rec_t;
  -- Record alias with a `field'subtype` subtype: `law.addr` selects a slice.
  alias law : defaults.wa'subtype is defaults.wa;
begin
  defaults.wa.addr <= bus.wa.addr;
  defaults.wa.data <= bus.wd.data;
end architecture;

entity tb_view is
end entity;

architecture sim of tb_view is
  signal clk : std_logic := '0';
  signal bus : work.view_pkg.rec_t;
  -- External-name alias parses and lowers (placeholder), no E0401.
  alias ext is << signal ^.u0.defaults.wa.addr : std_logic_vector(7 downto 0) >>;
begin
  u0 : entity work.dut
    port map (bus => bus, clk => clk);
  clk <= not clk after 5 ns;
  process
  begin
    bus.wa.addr <= x"11";
    bus.wd.data <= x"0000_00ff";
    wait for 20 ns;
    assert work.dut.ADDR_W = 8
      report "FAIL: entity-declarative constant not visible in architecture"
      severity error;
    report "PASS: vhdl2019 views, record aliases, entity context";
    std.env.finish;
    wait;
  end process;
end architecture;
