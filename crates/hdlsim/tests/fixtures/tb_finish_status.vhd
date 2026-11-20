-- Exit-code fixture: the argument to std.env.finish must become the
-- simulator's process exit code.
entity tb_finish_status is
end entity;
architecture sim of tb_finish_status is
begin
    process
    begin
        report "PASS: finish status fixture";
        std.env.finish(3);
        wait;
    end process;
end architecture;
