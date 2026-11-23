-- Regression fixture (invented names): formals may carry default values.
-- VHDL style puts the defaults on the package-spec interface; the package
-- body repeats the formals without them. A call may omit the corresponding
-- trailing actuals, which then take the recorded defaults.
PACKAGE def_formals_pkg IS
  FUNCTION scaled(w : natural := 8; k : natural := 3) RETURN natural;
END PACKAGE def_formals_pkg;
