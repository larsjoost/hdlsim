-- Companion of def_formals_spec.vhd: the testbench. r_const is initialized
-- at lower time (const-eval path); the process assignments run through the
-- simulator's function call path.
USE work.def_formals_pkg.all;

ENTITY tb_def_formals IS
END ENTITY tb_def_formals;

ARCHITECTURE sim OF tb_def_formals IS
  SIGNAL r_const : natural := scaled(7);   -- 7*10 + 3 (omitted k takes its default)
BEGIN
  PROCESS
    VARIABLE v_run : natural := 0;
  BEGIN
    v_run := scaled(7);                    -- runtime call, omitted defaulted formal
    ASSERT r_const = 73 REPORT "const-eval: defaulted formal not filled" SEVERITY error;
    ASSERT v_run = 73 REPORT "runtime: defaulted formal not filled" SEVERITY error;
    ASSERT scaled(7, 5) = 75 REPORT "explicit actuals broken" SEVERITY error;
    ASSERT scaled = 83 REPORT "all-defaults call broken" SEVERITY error;
    WAIT;
  END PROCESS;
END ARCHITECTURE sim;
