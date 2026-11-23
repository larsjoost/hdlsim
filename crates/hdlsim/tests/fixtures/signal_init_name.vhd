-- Regression fixture (invented names): a signal or variable initializer may
-- be a bare name (a constant reference). The initializer follows ':=' in the
-- source; declaration lowering must look past the ':=' instead of skipping
-- Name nodes, which made the initializer silently vanish.
ENTITY tb_init_name IS
END ENTITY tb_init_name;

ARCHITECTURE sim OF tb_init_name IS
  CONSTANT c_width : natural := 5;
  SIGNAL s_name : natural := c_width;   -- bare-name initializer
  SIGNAL s_lit : natural := 5;          -- literal initializer (control)
BEGIN
  PROCESS
    VARIABLE v_name : natural := c_width;
  BEGIN
    ASSERT s_name = 5 REPORT "bare-name signal initializer lost" SEVERITY error;
    ASSERT s_lit = 5 REPORT "literal signal initializer lost" SEVERITY error;
    ASSERT v_name = 5 REPORT "bare-name variable initializer lost" SEVERITY error;
    WAIT;
  END PROCESS;
END ARCHITECTURE sim;
