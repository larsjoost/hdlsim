-- ieee.std_logic_unsigned — embedded subset of the IEEE 1076.3 package.
-- Unsigned arithmetic/comparison/shift functions are kernel builtins in the
-- lowerer (matched by name); these declarations make the names resolve.
package std_logic_unsigned is

    function "+"  (l, r : unsigned) return unsigned;
    function "-"  (l, r : unsigned) return unsigned;
    function "*"  (l, r : unsigned) return unsigned;
    function "/"  (l, r : unsigned) return unsigned;
    function "mod" (l, r : unsigned) return unsigned;
    function "rem" (l, r : unsigned) return unsigned;
    function "abs" (l : unsigned) return unsigned;
    function "="  (l, r : unsigned) return boolean;
    function "/=" (l, r : unsigned) return boolean;
    function "<"  (l, r : unsigned) return boolean;
    function "<=" (l, r : unsigned) return boolean;
    function ">"  (l, r : unsigned) return boolean;
    function ">=" (l, r : unsigned) return boolean;

    function to_integer  (arg : unsigned) return natural;
    function resize      (arg : unsigned; new_size : in natural) return unsigned;
    function shift_left  (arg : unsigned; count : in natural) return unsigned;
    function shift_right (arg : unsigned; count : in natural) return unsigned;

end package std_logic_unsigned;
