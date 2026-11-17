-- ieee.fixed_pkg — embedded subset of the IEEE 1076.3 fixed-point package.
-- The ufixed/sfixed types are simulated as f64 real values (the left/right
-- bounds are not modeled); the conversion/operator functions below resolve
-- to real arithmetic in the lowerer/simulator.
package fixed_pkg is

    type ufixed is range <>;
    type sfixed is range <>;

    function to_ufixed (arg : real; left_index, right_index : in integer) return ufixed;
    function to_ufixed (arg : ufixed; left_index, right_index : in integer) return ufixed;
    function to_ufixed (arg : unsigned; left_index, right_index : in integer) return ufixed;
    function to_ufixed (arg : signed; left_index, right_index : in integer) return ufixed;
    function to_ufixed (arg : std_logic_vector; left_index, right_index : in integer) return ufixed;

    function to_sfixed (arg : real; left_index, right_index : in integer) return sfixed;
    function to_sfixed (arg : sfixed; left_index, right_index : in integer) return sfixed;
    function to_sfixed (arg : unsigned; left_index, right_index : in integer) return sfixed;
    function to_sfixed (arg : signed; left_index, right_index : in integer) return sfixed;
    function to_sfixed (arg : std_logic_vector; left_index, right_index : in integer) return sfixed;

    function to_real (arg : ufixed) return real;
    function to_real (arg : sfixed) return real;

    function resize (arg : ufixed; left_index, right_index : in integer) return ufixed;
    function resize (arg : sfixed; left_index, right_index : in integer) return sfixed;

end package fixed_pkg;
