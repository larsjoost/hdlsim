-- Signals, processes with sensitivity lists, process(all), wait, report.
entity tb is end entity;
architecture sim of tb is
  signal a : bit := '0';
  signal b : bit := '0';
  signal seen : bit := '0';
begin
  process(all)
  begin
    if a = '1' and b = '1' then
      seen <= '1';
    end if;
  end process;
  stim: process
  begin
    a <= '1'; b <= '1';
    wait for 1 ns;
    if seen = '1' then report "PASS"; else report "FAIL"; end if;
    wait;
  end process;
end architecture;
