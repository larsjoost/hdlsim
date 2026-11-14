entity tb_pass is
end entity;
architecture sim of tb_pass is
begin
    process
    begin
        report "PASS: vhdl pass fixture";
        std.env.finish;
        wait;
    end process;
end architecture;
