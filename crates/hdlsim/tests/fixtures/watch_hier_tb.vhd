-- Regression fixture for the --watch name-discovery paths: two-level
-- hierarchy so --list-signals shows hierarchical names and an unmatched
-- --watch name gets a "did you mean" suggestion plus the "N signals end
-- in '.cnt'" hint. Only elaboration must succeed — every test using this
-- fixture exits before simulation starts (listing) or errors out (bad
-- watch name).

entity counter8 is
end entity;

architecture rtl of counter8 is
  signal cnt : integer := 0;
  signal tick : bit := '0';
begin

  step : process (tick)
  begin
    if tick'event and tick = '1' then
      cnt <= cnt + 1;
    end if;
  end process;

end architecture;

entity watch_hier_tb is
end entity;

architecture sim of watch_hier_tb is
  signal clk : bit := '0';
  signal en : bit := '0';
begin

  dut : entity work.counter8;

  drive : process
  begin
    en <= '1';
    wait for 10 ns;
  end process;

end architecture;
