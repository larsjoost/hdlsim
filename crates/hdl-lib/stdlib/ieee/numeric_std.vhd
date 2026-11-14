-- ieee.numeric_std — embedded subset of the IEEE 1076.3 package.
-- unsigned/signed are vector types over std_logic; the arithmetic and
-- comparison functions are kernel builtins in the lowerer, so these
-- declarations exist to make the names resolve through the normal pipeline.
package numeric_std is

    type unsigned is array (natural range <>) of std_logic;
    type signed   is array (natural range <>) of std_logic;

    -- Arithmetic on unsigned.
    function "+"   (l, r : unsigned) return unsigned;
    function "-"   (l, r : unsigned) return unsigned;
    function "*"   (l, r : unsigned) return unsigned;
    function "/"   (l, r : unsigned) return unsigned;
    function "mod" (l, r : unsigned) return unsigned;
    function "rem" (l, r : unsigned) return unsigned;
    function "abs" (l : unsigned) return unsigned;

    -- Arithmetic on signed.
    function "+"   (l, r : signed) return signed;
    function "-"   (l, r : signed) return signed;
    function "*"   (l, r : signed) return signed;
    function "abs" (l : signed) return signed;

    -- Relational operators.
    function "="  (l, r : unsigned) return boolean;
    function "/=" (l, r : unsigned) return boolean;
    function "<"  (l, r : unsigned) return boolean;
    function "<=" (l, r : unsigned) return boolean;
    function ">"  (l, r : unsigned) return boolean;
    function ">=" (l, r : unsigned) return boolean;
    function "="  (l, r : signed) return boolean;
    function "<"  (l, r : signed) return boolean;
    function ">"  (l, r : signed) return boolean;

    -- Resizing and conversions.
    function resize    (arg : unsigned; new_size : natural) return unsigned;
    function resize    (arg : signed; new_size : natural) return signed;
    function to_integer (arg : unsigned) return natural;
    function to_integer (arg : signed) return integer;
    function to_unsigned (arg : natural; size : natural) return unsigned;
    function to_signed   (arg : integer; size : natural) return signed;

end package numeric_std;
