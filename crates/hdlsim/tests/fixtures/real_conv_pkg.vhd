-- Regression fixture (invented names): `real(x)` is a value conversion
-- (LRM 8.6). A bit-identical passthrough made an integer-argument overload
-- whose body calls the real-argument overload via `real(x)` call ITSELF
-- with the same integer — infinite recursion, caught only by the call-depth
-- guard.
PACKAGE real_conv_pkg IS
  FUNCTION scaled(x : integer) RETURN integer;
  FUNCTION scaled(r : real; k : integer := 2) RETURN integer;
END PACKAGE real_conv_pkg;

PACKAGE BODY real_conv_pkg IS
  FUNCTION scaled(x : integer) RETURN integer IS
  BEGIN
    RETURN scaled(real(x));          -- must reach the real-argument overload
  END FUNCTION scaled;

  FUNCTION scaled(r : real; k : integer) RETURN integer IS
  BEGIN
    RETURN integer(r * 10.0) + k;
  END FUNCTION scaled;
END PACKAGE BODY real_conv_pkg;
