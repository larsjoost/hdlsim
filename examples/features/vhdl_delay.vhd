-- Signal scheduling with after, delayed waveform elements, wait for.
entity tb is end entity;
architecture sim of tb is
  signal s : bit := '0';
  signal cnt : integer := 0;
begin
  s <= '1' after 2 ns, '0' after 4 ns;
  process(s)
  begin
    cnt <= cnt + 1;
  end process;
  process
  begin
    wait for 5 ns;
    if s = '0' and cnt = 3 then report "PASS"; else report "FAIL"; end if;
    wait;
  end process;
end architecture;
