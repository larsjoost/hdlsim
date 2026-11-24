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

    -- VHDL-2019 date/time API (decl-only; not simulated).
    type time_record is record
      time_value_sec  : integer;
      time_value_nsec : integer;
      time_second     : integer;
      time_minute     : integer;
      time_hour       : integer;
      time_day        : integer;
      time_month      : integer;
      time_year       : integer;
      time_zone       : integer;
    end record;
    constant EPOCH : real := 0.0;
    impure function LOCALTIME return time_record;
    impure function GMTIME return time_record;
    impure function LOCALTIME (timer : real) return time_record;
    impure function GMTIME (timer : real) return time_record;
    impure function LOCALTIME (trec : time_record) return time_record;
    impure function GMTIME (trec : time_record) return time_record;
    impure function EPOCH (trec : time_record) return real;
    function TIME_TO_SECONDS (trec : time_record) return real;
    function SECONDS_TO_TIME (secs : real) return time_record;
    function TO_STRING (value : time_record) return string;
    function TO_STRING (value : time_record; field : natural) return string;

end package env;
