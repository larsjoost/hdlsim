-- Self-checking: a status passed as a procedure argument flows through
-- nested procedure frames into `std.env.finish(status)` — the deepest call
-- receives the parameter via a bare-name argument and exits with it.
entity tb_proc_param_finish is
end entity;

architecture sim of tb_proc_param_finish is
  procedure quit(code : in integer) is
  begin
    std.env.finish(code);
  end procedure;

  procedure wrap(code : in integer) is
  begin
    quit(code);
  end procedure;
begin
  process
  begin
    wrap(2);
    wait;
  end process;
end architecture;
