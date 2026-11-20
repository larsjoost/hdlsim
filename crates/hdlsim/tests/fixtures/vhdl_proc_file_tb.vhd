-- Self-checking: file_open/write/writeline/file_close on a procedure-local
-- file. write_mode truncates on every open, so after the second call the
-- file holds only the last message.
library ieee;
use std.textio.all;

entity tb_proc_file is
end entity;

architecture sim of tb_proc_file is
  procedure log_result(msg : in string) is
    file f      : text;
    variable l  : line;
    variable st : file_open_status;
  begin
    file_open(st, f, "tests/out/proc_file_log.txt", write_mode);
    write(l, msg);
    writeline(f, l);
    file_close(f);
  end procedure;
begin
  process
  begin
    log_result("first");
    log_result("second");
    report "PASS: vhdl proc file";
    wait;
  end process;
end architecture;
