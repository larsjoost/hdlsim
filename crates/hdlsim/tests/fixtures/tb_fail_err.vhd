entity tb_fail_err is
end entity;
architecture sim of tb_fail_err is
begin
    process
    begin
        report "FAIL: vhdl severity error fixture" severity error;
        std.env.finish;
        wait;
    end process;
end architecture;
