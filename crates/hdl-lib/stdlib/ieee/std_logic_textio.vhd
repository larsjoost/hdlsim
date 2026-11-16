-- ieee.std_logic_textio — embedded subset of the IEEE 1076.3 package.
-- Text I/O is not simulated; these declarations only make the names resolve
-- so designs that `use ieee.std_logic_textio.all;` compile and lower.
package std_logic_textio is

    type line is access string;

    procedure write (l : inout line; value : in std_ulogic);
    procedure write (l : inout line; value : in std_ulogic_vector);
    procedure write (l : inout line; value : in unsigned);
    procedure write (l : inout line; value : in signed);
    procedure read  (l : inout line; value : out std_ulogic);
    procedure read  (l : inout line; value : out std_ulogic_vector);

end package std_logic_textio;
