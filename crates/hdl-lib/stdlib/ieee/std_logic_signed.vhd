-- ieee.std_logic_signed — embedded subset of the IEEE 1076.3 package.
-- Signed arithmetic/comparison/shift functions are kernel builtins in the
-- lowerer (matched by name); these declarations make the names resolve.
package std_logic_signed is

    function "+"  (l, r : signed) return signed;
    function "-"  (l, r : signed) return signed;
    function "*"  (l, r : signed) return signed;
    function "abs" (l : signed) return signed;
    function "="  (l, r : signed) return boolean;
    function "/=" (l, r : signed) return boolean;
    function "<"  (l, r : signed) return boolean;
    function "<=" (l, r : signed) return boolean;
    function ">"  (l, r : signed) return boolean;
    function ">=" (l, r : signed) return boolean;

    function to_integer  (arg : signed) return integer;
    function resize      (arg : signed; new_size : in natural) return signed;
    function shift_left  (arg : signed; count : in natural) return signed;
    function shift_right (arg : signed; count : in natural) return signed;

end package std_logic_signed;
