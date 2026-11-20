-- A `wait until` whose condition is already true at park time must NOT resume
-- without an event on the condition's signals (LRM 8.1): the condition is only
-- evaluated at events, never when the wait executes. Unrelated signal events
-- (clock) must not wake it either.
entity tb_wu_notrig is
end entity;

architecture sim of tb_wu_notrig is
  signal idle : bit := '0';   -- never toggles
  signal clock : bit := '0';  -- toggles every 10 ns
begin

  clock <= not clock after 10 ns;

  main : process
  begin
    wait for 10 ns;
    wait until idle /= '1';
    report "FAIL: pre-true wait resumed" severity error;
  end process;

  guard : process
  begin
    wait for 60 ns;
    report "PASS: pre-true wait stays parked";
    wait;
  end process;

end architecture;
