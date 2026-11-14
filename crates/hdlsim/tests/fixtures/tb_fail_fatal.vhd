entity tb_fail_fatal is
end entity;
architecture sim of tb_fail_fatal is
begin
    process
    begin
        assert false report "FAIL: vhdl severity failure fixture" severity failure;
        wait;
    end process;
end architecture;
