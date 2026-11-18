-- std.textio — embedded subset of the IEEE 1076 std.textio package.
-- hdlsim simulates the line buffer (`line`), the write/read procedures and
-- readline/writeline against real files (`file f : text open read_mode is
-- "path";`), `input` (stdin) and `output` (stdout).
package textio is

    type text is file of string;
    type line is access string;

    type side is (right, left);  -- for justifying output data within fields
    subtype width is natural;    -- for specifying widths of output fields

    file input : text open read_mode is "stdin";
    file output : text open write_mode is "stdout";

    procedure readline (file f : text; l : inout line);
    procedure writeline (file f : text; l : in line);

    procedure write (l : inout line; value : in string);
    procedure write (l : inout line; value : in integer);
    procedure write (l : inout line; value : in natural);
    procedure write (l : inout line; value : in positive);
    procedure write (l : inout line; value : in boolean);
    procedure write (l : inout line; value : in character);
    procedure write (l : inout line; value : in bit);
    procedure write (l : inout line; value : in time);
    procedure write (l : inout line; value : in real);

    procedure read (l : inout line; value : out string);
    procedure read (l : inout line; value : out integer);
    procedure read (l : inout line; value : out natural);
    procedure read (l : inout line; value : out boolean);
    procedure read (l : inout line; value : out character);
    procedure read (l : inout line; value : out bit);

    function endfile (file f : text) return boolean;

end package textio;
