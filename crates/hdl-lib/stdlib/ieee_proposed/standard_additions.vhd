-- ieee_proposed.standard_additions — embedded subset of the pre-2008
-- ieee_proposed standard_additions package: the VHDL-2008 string-conversion
-- family that later moved into the standard packages (std.standard /
-- ieee.std_logic_1164). The functions are kernel builtins resolved by name,
-- so these declarations exist to make the names resolve through the normal
-- pipeline.
package standard_additions is

    function to_string (value : string) return string;
    function to_string (value : character) return string;
    function to_string (value : boolean) return string;
    function to_string (value : bit) return string;
    function to_string (value : integer) return string;
    function to_string (value : natural) return string;
    function to_string (value : positive) return string;
    function to_string (value : real) return string;
    function to_string (value : time) return string;
    function to_string (value : std_logic) return string;
    function to_string (value : std_logic_vector) return string;
    function to_string (value : unsigned) return string;
    function to_string (value : signed) return string;

    function to_hstring (value : std_logic_vector) return string;
    function to_hstring (value : unsigned) return string;
    function to_hstring (value : signed) return string;
    function to_ostring (value : std_logic_vector) return string;
    function to_ostring (value : unsigned) return string;
    function to_ostring (value : signed) return string;
    function to_bstring (value : std_logic_vector) return string;
    function to_bstring (value : unsigned) return string;
    function to_bstring (value : signed) return string;

end package standard_additions;
