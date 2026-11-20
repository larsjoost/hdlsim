-- `time` values are femtoseconds and must keep full 64-bit magnitude: 30 ms is
-- 3e10 fs and overflows 32 bits (truncating to ~4.3 us). Covers both a `time`
-- constant in `wait for <expr>` and a `now` comparison.
entity tb_time_width is
end entity;

architecture sim of tb_time_width is
  constant C_TIMEOUT : time := 30 ms;
begin

  main : process
  begin
    wait for C_TIMEOUT;
    if now /= 30 ms then
      report "FAIL: time constant truncated" severity error;
    else
      report "PASS: time constants keep 64-bit fs";
    end if;
    wait;
  end process;

end architecture;
