-- ieee.float_generic_pkg — embedded subset of the IEEE 1076.3 / 2008
-- float_generic_pkg package (declared; bodies are not simulated). The float
-- types lower to f64 real values in the simulator.
-- Generic package: `package p is new ieee.float_generic_pkg generic map (...)`.
library ieee;
use ieee.fixed_float_types.all;

package float_generic_pkg is

    generic (
        round_style     : round_type := round_nearest;
        denormalize     : boolean  := false;
        guard_bits      : natural  := 0;
        no_warning      : boolean  := false
    );

    type float is range <>;

    subtype float32 is float(8 downto -149);
    subtype float64 is float(11 downto -1074);

    function to_float (arg : real; size : natural) return float;
    function to_float (arg : integer; size : natural) return float;
    function to_float (arg : float; left_index, right_index : in integer) return float;
    function to_real (arg : float) return real;
    function to_integer (arg : float) return integer;

    function "+" (arg : float) return float;
    function "-" (arg : float) return float;
    function "+" (l, r : float) return float;
    function "-" (l, r : float) return float;
    function "*" (l, r : float) return float;
    function "/" (l, r : float) return float;
    function "=" (l, r : float) return boolean;
    function "/=" (l, r : float) return boolean;
    function "<" (l, r : float) return boolean;
    function "<=" (l, r : float) return boolean;
    function ">" (l, r : float) return boolean;
    function ">=" (l, r : float) return boolean;

end package float_generic_pkg;
