-- ieee.float_pkg — embedded subset of the IEEE 1076.3 floating-point package.
-- The float type is simulated as an f64 real value (exponent/fraction widths
-- are not modeled); the conversion functions resolve to real arithmetic.
package float_pkg is

    type float is range <>;

    function to_float (arg : real) return float;
    function to_float (arg : real; exp_width, frac_width : in integer) return float;
    function to_float (arg : integer) return float;
    function to_float (arg : std_logic_vector; exp_width, frac_width : in integer) return float;

    function to_real (arg : float) return real;
    function to_integer (arg : float) return integer;

    function resize (arg : float; exp_width, frac_width : in integer) return float;

end package float_pkg;
