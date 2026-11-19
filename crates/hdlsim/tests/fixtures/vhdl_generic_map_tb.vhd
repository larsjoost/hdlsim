-- Self-checking testbench for instantiation-time `generic map` binding.
-- The child is lowered with default/absent generics first; the generic map
-- must re-bind them at elaboration (elab-time variant re-lowering), so the
-- child's port widths and generate ranges use the mapped values.
library ieee;
use ieee.std_logic_1164.all;
use std.env.all;

entity gmap_counter is
  generic (WIDTH : integer := 4);
  port (clk : in std_logic; q : out std_logic_vector(WIDTH-1 downto 0));
end entity;

architecture rtl of gmap_counter is
  signal cnt : unsigned(WIDTH-1 downto 0) := (others => '0');
begin
  process(clk)
  begin
    if rising_edge(clk) then
      cnt <= cnt + 1;
    end if;
  end process;
  q <= std_logic_vector(cnt);
end architecture;

-- Generic without a default: the for-generate range is only known through
-- the generic map at the instantiation.
entity gmap_nib is
  generic (N : natural);
  port (d : in std_logic_vector(4*N-1 downto 0);
        q : out std_logic_vector(8*N-1 downto 0));
end entity;

architecture rtl of gmap_nib is
begin
  g : for i in 0 to N-1 generate
    q(8*i+7 downto 8*i) <= x"3" & d(4*i+3 downto 4*i);
  end generate;
end architecture;

entity tb_generic_map is
end entity;

architecture sim of tb_generic_map is
  signal clk : std_logic := '0';
  signal q8  : std_logic_vector(7 downto 0);
  signal din : std_logic_vector(7 downto 0) := x"A5";
  signal dout : std_logic_vector(15 downto 0);
begin
  process
  begin
    clk <= not clk;
    wait for 5 ns;
  end process;

  -- WIDTH => 8: must not wrap at 4 bits (q would read 04 after 20 clocks).
  u_cnt : entity work.gmap_counter generic map (WIDTH => 8)
    port map (clk => clk, q => q8);

  -- N => 2 with no default: the generate must unroll to two slices.
  u_nib : entity work.gmap_nib generic map (N => 2)
    port map (d => din, q => dout);

  process
  begin
    for i in 1 to 20 loop
      wait until rising_edge(clk);
    end loop;
    wait for 1 ns;
    assert q8 = x"14" report "WIDTH=>8 not bound, q8=" & to_hstring(q8) severity error;
    assert dout = x"3A35" report "N=>2 generate not bound, dout=" & to_hstring(dout) severity error;
    report "PASS: vhdl generic map binding";
    std.env.finish;
  end process;
end architecture;
