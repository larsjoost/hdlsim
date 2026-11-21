-- Runaway loop: a `loop` whose body never waits must be stopped by the
-- interpreter step budget (HDLSIM_MAX_PROC_STEPS) with a diagnostic naming
-- the process, and the run must exit nonzero — not hang forever.
entity tb_runaway is
end entity;

architecture sim of tb_runaway is
  signal clk : std_ulogic := '0';
begin
  spinner : PROCESS IS
  BEGIN
    LOOP
      REPORT "spinning";
    END LOOP;
  END PROCESS spinner;
end architecture;
