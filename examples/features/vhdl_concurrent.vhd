-- Concurrent signal assignment, conditional when/else, selected with/select.
entity tb is end entity;
architecture sim of tb is
  signal sel : bit := '1';
  signal a, b : bit := '0';
  signal y_cond : bit;
  signal y_sel : bit;
  signal z : bit;
begin
  y_cond <= '1' when sel = '1' else '0';
  with sel select
    y_sel <= '1' when '1',
             '0' when others;
  z <= a and b;
  a <= '1';
  b <= '1';
  process
  begin
    wait for 1 ns;
    if y_cond = '1' and y_sel = '1' and z = '1' then report "PASS"; else report "FAIL"; end if;
    wait;
  end process;
end architecture;
