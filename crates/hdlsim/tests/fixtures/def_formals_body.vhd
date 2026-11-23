-- Companion of def_formals_spec.vhd: the body's interface repeats the
-- formals without their defaults (standard VHDL style).
PACKAGE BODY def_formals_pkg IS
  FUNCTION scaled(w : natural; k : natural) RETURN natural IS
  BEGIN
    RETURN w * 10 + k;
  END FUNCTION scaled;
END PACKAGE BODY def_formals_pkg;
