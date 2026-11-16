-- std.env — embedded subset of the IEEE 1076 VHDL-2008 environment package.
-- `finish` / `stop` are kernel builtins in the simulator; these declarations
-- make `use std.env.all;` resolve through the normal pipeline.
package env is

    procedure finish;
    procedure finish (status : in integer);
    procedure stop;
    procedure stop (status : in integer);

end package env;
