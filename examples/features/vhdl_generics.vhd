-- Generics with defaults and overrides via configuration-free instantiation.
entity counter is
  generic (WIDTH : integer := 8);
  port (clk : in bit; q : out bit_vector(WIDTH-1 downto 0));
end entity;
architecture rtl of counter is
  signal qq : bit_vector(WIDTH-1 downto 0) := (others => '0');
begin
  process(clk)
  begin
    if clk = '1' then
      qq <= bit_vector(unsigned(qq) + 1);
    end if;
  end process;
  q <= qq;
end architecture;

entity tb is end entity;
architecture sim of tb is
  signal clk : bit := '0';
  signal q4 : bit_vector(3 downto 0);
  signal q8 : bit_vector(7 downto 0);
begin
  u4: entity work.counter generic map (WIDTH => 4) port map (clk => clk, q => q4);
  u8: entity work.counter port map (clk => clk, q => q8);
  clk <= not clk after 5 ns;
  process
  begin
    wait for 11 ns;
    if q4 = "0001" and q8 = x"01" then report "PASS"; else report "FAIL"; end if;
    wait;
  end process;
end architecture;
