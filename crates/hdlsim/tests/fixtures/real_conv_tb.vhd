-- Companion of real_conv_pkg.vhd: scaled(2) must route integer -> real
-- overload (2*10 + 2 = 22) instead of recursing into the integer overload.
USE work.real_conv_pkg.all;

ENTITY tb_real_conv IS
END ENTITY tb_real_conv;

ARCHITECTURE sim OF tb_real_conv IS
  SIGNAL r_const : integer := scaled(2);   -- const-eval path
BEGIN
  PROCESS
    VARIABLE v_run : integer := 0;
  BEGIN
    v_run := scaled(2);                    -- runtime call path
    ASSERT r_const = 22 REPORT "const-eval: real(x) conversion dropped" SEVERITY error;
    ASSERT v_run = 22 REPORT "runtime: real(x) conversion dropped" SEVERITY error;
    WAIT;
  END PROCESS;
END ARCHITECTURE sim;
