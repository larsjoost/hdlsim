-- Companion of def_formals_spec.vhd: spec and body in one file (the
-- same-lower-instance path).
PACKAGE def_formals_pkg IS
  FUNCTION scaled(w : natural := 8; k : natural := 3) RETURN natural;
END PACKAGE def_formals_pkg;

PACKAGE BODY def_formals_pkg IS
  FUNCTION scaled(w : natural; k : natural) RETURN natural IS
  BEGIN
    RETURN w * 10 + k;
  END FUNCTION scaled;
END PACKAGE BODY def_formals_pkg;
