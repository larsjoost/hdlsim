-- Fixture: for-loop parameter scoping (LRM 1076, loop statement: the loop
-- parameter is a constant whose scope is the loop itself, and it shadows any
-- outer object of the same name). A regression shared one counter wire per
-- parameter *name* across the whole design, so concurrent loops clobbered
-- each other as soon as a body suspended on a wait, and a loop parameter
-- overwrote an architecture-level object of the same name.
--
-- Covers three shapes:
--   * two concurrent loops with the same parameter name, variable-derived
--     ranges, suspending bodies — each must run its own iteration count;
--   * nested loops with the same parameter name — the inner shadows, the
--     outer keeps counting;
--   * a loop parameter must never write the architecture signal of the
--     same name.
entity tb_for_param_scope is
end entity;

architecture sim of tb_for_param_scope is
  signal clk : std_ulogic := '0';
  signal i   : integer := 100;
begin

  clk_p : process is
  begin
    for c in 0 to 19 loop
      clk <= '0';
      wait for 5 ns;
      clk <= '1';
      wait for 5 ns;
    end loop;
    wait;
  end process;

  -- Two concurrent beat generators: same parameter name, variable-derived
  -- ranges, bodies that suspend.
  gen_a : process is
    variable beats : integer := 3;
    variable count : integer := 0;
  begin
    for i in 0 to beats - 1 loop
      wait until rising_edge(clk);
      count := count + 1;
    end loop;
    assert count = 3
      report "gen_a ran " & integer'image(count) & " of 3 beats"
      severity failure;
    report "PASS gen_a";
    wait;
  end process;

  gen_b : process is
    variable beats : integer := 2;
    variable count : integer := 0;
  begin
    for i in 0 to beats - 1 loop
      wait until rising_edge(clk);
      count := count + 1;
    end loop;
    assert count = 2
      report "gen_b ran " & integer'image(count) & " of 2 beats"
      severity failure;
    report "PASS gen_b";
    wait;
  end process;

  -- Nested loops with the same parameter name: the inner one shadows the
  -- outer. 2 outer iterations x (2 inner + 10) = 24.
  nest : process is
    variable count : integer := 0;
  begin
    for i in 0 to 1 loop
      for i in 0 to 1 loop
        wait for 1 ns;
        count := count + 1;
      end loop;
      count := count + 10;
    end loop;
    assert count = 24
      report "nested loops counted " & integer'image(count) & " of 24"
      severity failure;
    report "PASS nest";
    wait;
  end process;

  -- No loop may write the architecture signal `i`.
  guard : process is
  begin
    wait for 150 ns;
    assert i = 100
      report "a loop wrote the architecture signal i (= "
        & integer'image(i) & ")" severity failure;
    report "PASS guard";
    wait;
  end process;
end architecture;
