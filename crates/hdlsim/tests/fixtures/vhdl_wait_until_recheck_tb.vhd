-- `wait until cond` re-checks the condition on every wake: a falling edge of
-- the sampled signal must not resume the process (LRM 8.1). Regression: the
-- condition used to be discarded after lowering, so any event woke the
-- process.
entity tb_wu_recheck is
end entity;

architecture sim of tb_wu_recheck is
  signal pulse : bit := '0';
begin

  pulse <= '1' after 20 ns, '0' after 40 ns, '1' after 60 ns;

  main : process
  begin
    wait until pulse = '1';
    if now /= 20 ns then
      report "FAIL: first wake not at rising edge" severity error;
    end if;
    wait until pulse = '1';
    if now /= 60 ns then
      report "FAIL: second wake not at second rising edge" severity error;
    else
      report "PASS: wait-until re-checks condition";
    end if;
    wait;
  end process;

end architecture;
