library ieee ;
use ieee.std_logic_1164.all ;
library r13lib ;
context r13lib.ctx ;

-- DUT with a VHDL-2019 mode-view port, an entity-level signal with a
-- constrained record subtype, and an entity-level alias to a view member.
entity dut is
  port (
    busp : view r13lib.ifacepkg.mgrview of r13lib.ifacepkg.bus_t ;
    clk  : in  std_logic
  ) ;
  signal transrec : r13lib.ifacepkg.bus_t (
    wa (addr (15 downto 0)),
    wd (7 downto 0)
  ) ;
  constant aw : integer := busp.wa.addr'length ;
  alias awalias : busp.wa'subtype is busp.wa ;
end entity ;

architecture a of dut is
  signal baud : time := 8 us ;
begin
  process
    variable localaw : busp.wa'subtype ;
    variable localwd : busp.wd'subtype ;
  begin
    localaw.addr := (others => '0') ;
    assert aw = 32 report "view-port member attribute" severity failure ;
    assert awalias.addr'length = 32 report "entity alias to view member" severity failure ;
    assert transrec.wa.addr'length = 32 report "entity-level record signal member" severity failure ;
    assert localaw.id'length = 8 report "nested field width" severity failure ;
    assert localwd'length = 32 report "field'subtype variable" severity failure ;
    -- non-constant waveform delay: warning, transaction still applied
    baud <= 4 us after baud / 2 ;
    report "PASS: r13 dut view ports + entity-level decls" ;
    wait ;
  end process ;
end architecture ;

entity tb is end entity ;
architecture sim of tb is
  -- package instantiation naming a library-qualified generic package
  package mgr is new r13lib.genpkg generic map (elem_t => std_logic_vector, width => 16) ;
  signal busp : r13lib.ifacepkg.bus_t ;
  signal clk : std_logic := '0' ;
  signal half : time := 2 ns ;
begin
  dut1 : entity work.dut port map (busp => busp, clk => clk) ;
  clk <= not clk after 5 ns ;
  -- non-constant waveform delay (delay depends on a signal): warning only
  busp.wa.addr <= (others => '0') after half ;
  process
  begin
    wait for 1 ns ;
    mgr.sig <= (others => '1') ;
    assert mgr.sig'length = 16 report "pkg instance selected signal width" severity failure ;
    assert mgr.def = 16 report "pkg instance selected constant" severity failure ;
    report "PASS: r13 package instance selected path" ;
    std.env.finish ;
  end process ;
end architecture ;
