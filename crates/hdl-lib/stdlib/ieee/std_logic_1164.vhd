-- ieee.std_logic_1164 — embedded subset of the IEEE 1076.3 package.
-- Real VHDL source parsed by hdlsim's own front-end; the logic values and
-- the operator functions below are kernel builtins in the lowerer, so these
-- declarations exist to make the names resolve through the normal pipeline
-- (and to document the standard interface).
package std_logic_1164 is

    -- The nine VHDL logic values.
    type std_ulogic is ('U', 'X', '0', '1', 'Z', 'W', 'L', 'H', '-');

    -- The resolved subtype used for single signals.
    subtype std_logic is resolved std_ulogic;

    type std_ulogic_vector is array (natural range <>) of std_ulogic;
    type std_logic_vector is array (natural range <>) of std_logic;

    -- Logical operators on scalars.
    function "and"  (l : std_ulogic; r : std_ulogic) return std_ulogic;
    function "nand" (l : std_ulogic; r : std_ulogic) return std_ulogic;
    function "or"   (l : std_ulogic; r : std_ulogic) return std_ulogic;
    function "nor"  (l : std_ulogic; r : std_ulogic) return std_ulogic;
    function "xor"  (l : std_ulogic; r : std_ulogic) return std_ulogic;
    function "xnor" (l : std_ulogic; r : std_ulogic) return std_ulogic;
    function "not"  (l : std_ulogic) return std_ulogic;

    -- Logical operators on vectors.
    function "and"  (l : std_ulogic_vector; r : std_ulogic_vector) return std_ulogic_vector;
    function "or"   (l : std_ulogic_vector; r : std_ulogic_vector) return std_ulogic_vector;
    function "xor"  (l : std_ulogic_vector; r : std_ulogic_vector) return std_ulogic_vector;
    function "not"  (l : std_ulogic_vector) return std_ulogic_vector;

    -- Edge detection.
    function rising_edge  (signal s : std_logic) return boolean;
    function falling_edge (signal s : std_logic) return boolean;

    -- Conversions.
    function to_stdlogicvector  (s : bit_vector) return std_logic_vector;
    function to_stdulogicvector (s : bit_vector) return std_ulogic_vector;
    function to_bitvector       (s : std_logic_vector) return bit_vector;
    function to_x01             (s : std_ulogic) return std_ulogic;

    -- VHDL-2019: unresolved vector types (base of resolved vector subtypes).
    type unresolved_unsigned is array (natural range <>) of std_ulogic;
    type unresolved_signed is array (natural range <>) of std_ulogic;

end package std_logic_1164;
