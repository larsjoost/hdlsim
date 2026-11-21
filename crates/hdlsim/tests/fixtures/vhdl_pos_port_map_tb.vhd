-- A positional port map (`port map (clk, q)`) must connect both actuals.
-- Regression: lower_inst treated the first Name of a positional entry as the
-- formal of a named association, so every positional connection was silently
-- dropped and the child's ports floated (q stuck at 0, no PASS line).
entity pw is
  port (clk : in std_ulogic; q : out integer);
end entity;

architecture a of pw is
begin
  process (clk)
  begin
    if rising_edge(clk) then
      q <= q + 1;
    end if;
  end process;
end architecture;

entity tb_posmap is
end entity;

architecture sim of tb_posmap is
  signal clk : std_ulogic := '0';
  signal q : integer := 0;
begin
  u : entity pw port map (clk, q);
  clk <= not clk after 10 ns;

  check : process (q)
  begin
    if q >= 4 then
      report "PASS: positional port map";
    end if;
  end process check;
end architecture;
