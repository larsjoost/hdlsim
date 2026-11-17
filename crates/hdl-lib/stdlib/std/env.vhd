-- std.env — embedded subset of the IEEE 1076 VHDL-2008 environment package.
-- `finish` / `stop` are kernel builtins in the simulator; these declarations
-- make `use std.env.all;` resolve through the normal pipeline.
package env is

    procedure finish;
    procedure finish (status : in integer);
    procedure stop;
    procedure stop (status : in integer);

    function resolution_limit return delay_length;

    constant VHDL_VERSION : string := "2019";
    constant TOOL_TYPE    : string := "simulator";
    constant TOOL_VENDOR  : string := "hdlsim";
    constant TOOL_NAME    : string := "hdlsim";
    constant TOOL_EDITION : string := "";
    constant TOOL_VERSION : string := "0.2";

    -- VHDL-2019 source-location / call-path API (kernel builtins).
    type string_ptr is access string;
    type call_path_element is record
      file_name : string_ptr;
      file_line : positive;
    end record;
    type call_path_vector is array (natural range <>) of call_path_element;
    type call_path_vector_ptr is access call_path_vector;

    function FILE_NAME return string;
    function FILE_PATH return string;
    function FILE_LINE return string;
    impure function GET_CALL_PATH return call_path_vector_ptr;
    function TO_STRING (value : call_path_vector; separator : string) return string;
    function TO_STRING (value : call_path_vector_ptr; separator : string) return string;

end package env;
