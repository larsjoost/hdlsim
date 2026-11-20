-- The FOR timeout of `wait until cond for <expr>` is evaluated once, when the
-- wait executes, and fires even when the condition is already true and no
-- event ever occurs (LRM 8.1). Regression: a non-literal timeout expression
-- (`C_TIMEOUT - now`) used to be dropped, leaving the process parked forever.
entity tb_wu_timeout is
end entity;

architecture sim of tb_wu_timeout is
  constant C_TIMEOUT : time := 40 ns;
  signal idle : bit := '0';  -- never toggles
begin

  main : process
  begin
    wait for 10 ns;
    -- Pre-true condition, pure timeout wake: must resume at 40 ns.
    wait until idle /= '1' for C_TIMEOUT - now;
    if now /= 40 ns then
      report "FAIL: timeout at wrong time" severity error;
    else
      report "PASS: wait-until timeout";
    end if;
    wait;
  end process;

end architecture;
