-- `wait until cond for T` must wake on the condition's edge when it arrives
-- before the timeout (LRM 8.1). Regression: the FOR clause used to swallow the
-- condition and wake only at the timeout.
entity tb_wu_edge is
end entity;

architecture sim of tb_wu_edge is
  signal trig : bit := '0';
begin

  trig <= '1' after 30 ns;

  main : process
  begin
    wait for 10 ns;
    wait until trig = '1' for 100 ns;
    if now /= 30 ns then
      report "FAIL: woke at wrong time" severity error;
    else
      report "PASS: wait-until-for edge wake";
    end if;
    wait;
  end process;

end architecture;
