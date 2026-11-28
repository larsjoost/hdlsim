-- Fixture: process-variable scoping. A variable declared in a process
-- declarative part is visible only inside that process: a later process
-- reading the same name must resolve it to the architecture-level object,
-- not to the earlier process's private slot. A regression leaked process
-- variable name bindings into the shared lowerer namespace, silently
-- redirecting other processes' references to the first process's slot.
entity tb_procvar_scope is
end entity;

architecture sim of tb_procvar_scope is
  signal clk  : std_ulogic := '0';
  signal seed : integer := 7;
begin

  clk_p : process is
  begin
    for c in 0 to 9 loop
      clk <= '0';
      wait for 5 ns;
      clk <= '1';
      wait for 5 ns;
    end loop;
    wait;
  end process;

  -- Declares a local `seed` that shadows the architecture signal inside
  -- this process only.
  first : process is
    variable seed : integer := 99;
  begin
    assert seed = 99
      report "first must see its own local seed" severity failure;
    wait until rising_edge(clk);
    report "PASS first";
    wait;
  end process;

  -- Lowered after `first`: must still resolve `seed` to the architecture
  -- signal, not to first's local slot.
  second : process is
    variable seen : integer := 0;
  begin
    wait until rising_edge(clk);
    seen := seed;
    assert seen = 7
      report "second read seed=" & integer'image(seen)
        & ", expected the architecture signal 7" severity failure;
    report "PASS second";
    wait;
  end process;

  -- The signal itself keeps its declared value.
  guard : process is
  begin
    wait for 80 ns;
    assert seed = 7
      report "architecture signal seed was clobbered" severity failure;
    report "PASS guard";
    wait;
  end process;
end architecture;
