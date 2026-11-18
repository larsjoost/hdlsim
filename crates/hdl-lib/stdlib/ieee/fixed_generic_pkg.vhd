-- ieee.fixed_generic_pkg — embedded subset of the IEEE 1076.3 / 2008
-- fixed_generic_pkg package (declared; bodies are not simulated, like the
-- other stdlib packages). Types lower to f64 real values in the simulator.
-- Generic package: `package p is new ieee.fixed_generic_pkg generic map (...)`.
library ieee;
use ieee.fixed_float_types.all;

package fixed_generic_pkg is

    generic (
        fixed_round_style    : fixed_round_style_type    := fixed_round;
        fixed_overflow_style : fixed_overflow_style_type := fixed_saturate;
        fixed_guard_bits     : natural                   := 0;
        no_warning           : boolean                   := false
    );

    type ufixed is range <>;
    type sfixed is range <>;

    subtype ufixed32 is ufixed(31 downto 0);
    subtype sfixed32 is sfixed(31 downto 0);

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

    function "+" (arg : ufixed) return ufixed;
    function "-" (arg : ufixed) return ufixed;
    function "+" (l, r : ufixed) return ufixed;
    function "-" (l, r : ufixed) return ufixed;
    function "*" (l, r : ufixed) return ufixed;
    function "/" (l, r : ufixed) return ufixed;
    function "+" (l, r : sfixed) return sfixed;
    function "-" (l, r : sfixed) return sfixed;
    function "*" (l, r : sfixed) return sfixed;
    function "/" (l, r : sfixed) return sfixed;
    function "=" (l, r : ufixed) return boolean;
    function "/=" (l, r : ufixed) return boolean;
    function "<" (l, r : ufixed) return boolean;
    function "<=" (l, r : ufixed) return boolean;
    function ">" (l, r : ufixed) return boolean;
    function ">=" (l, r : ufixed) return boolean;
    function "=" (l, r : sfixed) return boolean;
    function "/=" (l, r : sfixed) return boolean;
    function "<" (l, r : sfixed) return boolean;
    function "<=" (l, r : sfixed) return boolean;
    function ">" (l, r : sfixed) return boolean;
    function ">=" (l, r : sfixed) return boolean;

end package fixed_generic_pkg;
