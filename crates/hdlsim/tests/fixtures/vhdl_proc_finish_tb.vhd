-- Self-checking: `report` and `std.env.finish(status)` inside a
-- user-declared procedure take effect — the status becomes the exit code,
-- and statements after the call in the same activation do not run.
entity tb_proc_finish is
end entity;

architecture sim of tb_proc_finish is
  procedure stop_now is
  begin
    report "PROC RAN" severity note;
    std.env.finish(3);
  end procedure;
begin
  process
  begin
    report "START";
    stop_now;
    report "AFTER FINISH";
    wait;
  end process;
end architecture;
