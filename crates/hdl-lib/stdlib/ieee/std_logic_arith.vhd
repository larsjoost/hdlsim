-- ieee.std_logic_arith — embedded subset of the IEEE 1076.3 package.
-- Real VHDL source parsed by hdlsim's own front-end; the arithmetic,
-- comparison, conversion and shift functions are kernel builtins in the
-- lowerer (matched by name), so these declarations exist to make the names
-- resolve through the normal pipeline and to document the interface.
package std_logic_arith is

    type unsigned is array (natural range <>) of std_logic;
    type signed   is array (natural range <>) of std_logic;

    -- Arithmetic.
    function "+"   (l, r : unsigned) return unsigned;
    function "-"   (l, r : unsigned) return unsigned;
    function "*"   (l, r : unsigned) return unsigned;
    function "/"   (l, r : unsigned) return unsigned;
    function "mod" (l, r : unsigned) return unsigned;
    function "rem" (l, r : unsigned) return unsigned;
    function "abs" (l : unsigned) return unsigned;
    function "+"   (l, r : signed) return signed;
    function "-"   (l, r : signed) return signed;
    function "*"   (l, r : signed) return signed;
    function "abs" (l : signed) return signed;

    -- Relational.
    function "="  (l, r : unsigned) return boolean;
    function "/=" (l, r : unsigned) return boolean;
    function "<"  (l, r : unsigned) return boolean;
    function "<=" (l, r : unsigned) return boolean;
    function ">"  (l, r : unsigned) return boolean;
    function ">=" (l, r : unsigned) return boolean;
    function "="  (l, r : signed) return boolean;
    function "/=" (l, r : signed) return boolean;
    function "<"  (l, r : signed) return boolean;
    function "<=" (l, r : signed) return boolean;
    function ">"  (l, r : signed) return boolean;
    function ">=" (l, r : signed) return boolean;

    -- Conversions.
    function conv_integer   (arg : unsigned) return integer;
    function conv_integer   (arg : signed) return integer;
    function conv_unsigned  (arg : integer; size : in integer) return unsigned;
    function conv_signed    (arg : integer; size : in integer) return signed;
    function conv_std_logic_vector (arg : unsigned; size : in integer) return std_logic_vector;
    function conv_std_logic_vector (arg : signed; size : in integer) return std_logic_vector;

    -- Shifts and rotates (unsigned).
    function shift_left  (arg : unsigned; count : in natural) return unsigned;
    function shift_right (arg : unsigned; count : in natural) return unsigned;
    function rotate_left (arg : unsigned; count : in natural) return unsigned;
    function rotate_right(arg : unsigned; count : in natural) return unsigned;
    function "sll" (arg : unsigned; count : in natural) return unsigned;
    function "srl" (arg : unsigned; count : in natural) return unsigned;
    function "rol" (arg : unsigned; count : in natural) return unsigned;
    function "ror" (arg : unsigned; count : in natural) return unsigned;

    -- Shifts and rotates (signed).
    function shift_left  (arg : signed; count : in natural) return signed;
    function shift_right (arg : signed; count : in natural) return signed;

end package std_logic_arith;
