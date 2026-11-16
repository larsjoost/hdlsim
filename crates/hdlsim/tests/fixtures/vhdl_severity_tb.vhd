-- Self-checking testbench for the std.standard severity_level literals
-- (note/warning/error/failure), used e.g. by vital_* assert helpers as
-- `MsgSeverity => ERROR`.
library ieee;
use std.env.all;

entity tb_severity is
end entity;

architecture sim of tb_severity is
begin
  process
    variable s : natural;
  begin
    s := ERROR;
    assert s = 2 report "ERROR literal failed" severity failure;
    if WARNING = 1 and NOTE = 0 and FAILURE = 3 then
      report "PASS: vhdl severity literals";
    else
      report "severity literals wrong" severity failure;
    end if;
    std.env.finish;
    wait;
  end process;
end architecture;
