-- ieee.math_real — embedded subset of the IEEE 1076 VHDL-2008 math_real
-- package. The constants are kernel-evaluable; the transcendental functions
-- are declared (calls lower to the simulator's real-math builtins).
package math_real is

    constant math_e        : real := 2.718281828459045;
    constant math_pi       : real := 3.141592653589793;
    constant math_2_pi     : real := 6.283185307179586;
    constant math_pi_2     : real := 1.570796326794897;
    constant math_pi_4     : real := 0.785398163397448;
    constant math_1_pi     : real := 0.318309886183791;
    constant math_2_pi_3   : real := 0.636619772367581;
    constant math_sqrt2    : real := 1.414213562373095;
    constant math_sqrt_1_2 : real := 0.707106781186548;
    constant math_log2_e   : real := 1.442695040888963;
    constant math_log10_e  : real := 0.434294481903252;
    constant math_ln2      : real := 0.693147180559945;
    constant math_ln10     : real := 2.302585092994046;
    -- VHDL-2019 additions (1076-2019 math_real):
    constant math_1_over_e    : real := 0.367879441171442;
    constant math_pi_over_2   : real := 1.570796326794897;
    constant math_pi_over_3   : real := 1.047197551196598;
    constant math_pi_over_4   : real := 0.785398163397448;
    constant math_3_pi_over_2 : real := 4.712388980384690;
    constant math_sqrt_pi     : real := 1.772453850905516;
    constant math_deg_to_rad  : real := 0.017453292519943;
    constant math_rad_to_deg  : real := 57.295779513082323;

    function sqrt  (x : real) return real;
    function log   (x : real) return real;
    function log2  (x : real) return real;
    function log10 (x : real) return real;
    function exp   (x : real) return real;
    function sin   (x : real) return real;
    function cos   (x : real) return real;
    function tan   (x : real) return real;
    function arcsin(x : real) return real;
    function arccos(x : real) return real;
    function arctan(y : real; x : real := 1.0) return real;
    function floor (x : real) return real;
    function ceil  (x : real) return real;

end package math_real;
