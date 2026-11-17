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

end package env;
