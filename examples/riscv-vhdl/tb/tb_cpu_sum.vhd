-- Integration test: run the "sum 1..10" program on the RV32I cpu and check
-- that dmem[0] = 55 (0x37).
--   0: addi x1, x0, 0     sum = 0
--   1: addi x2, x0, 1     i = 1
--   2: addi x3, x0, 11    limit = 11
--   3: add  x1, x1, x2    loop: sum += i
--   4: addi x2, x2, 1     i++
--   5: blt  x2, x3, -8    if i < 11 goto loop
--   6: sw   x1, 0(x0)     dmem[0] = sum
--   7: ebreak             halt
library ieee;
library rv32i;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;

entity tb_cpu_sum is
end entity;

architecture sim of tb_cpu_sum is
    signal clk : std_logic := '0';
    signal rst : std_logic := '1';
    signal halted : std_logic;
    signal imem_addr : unsigned(31 downto 0);
    signal imem_data : std_logic_vector(31 downto 0);
    signal dmem_addr : unsigned(31 downto 0);
    signal dmem_we : std_logic;
    signal dmem_wdata : unsigned(31 downto 0);
    signal dmem_rdata : std_logic_vector(31 downto 0);
    signal imem_we : std_logic := '0';
    signal imem_waddr : unsigned(7 downto 0) := x"00";
    signal imem_wdata : std_logic_vector(31 downto 0) := x"00000000";
    signal taddr : unsigned(7 downto 0) := x"00";
    signal tdata : std_logic_vector(31 downto 0);
    signal errors : integer := 0;
begin
    u_cpu : entity rv32i.cpu
        port map (clk => clk, rst => rst, halted => halted,
                  imem_addr => imem_addr, imem_data => imem_data,
                  dmem_addr => dmem_addr, dmem_we => dmem_we,
                  dmem_wdata => dmem_wdata, dmem_rdata => dmem_rdata);
    u_imem : entity rv32i.imem
        port map (clk => clk, we => imem_we, waddr => imem_waddr, wdata => imem_wdata,
                  raddr => imem_addr, rdata => imem_data);
    u_dmem : entity rv32i.dmem
        port map (clk => clk, we => dmem_we, waddr => dmem_addr, wdata => dmem_wdata,
                  raddr => dmem_addr, rdata => dmem_rdata, taddr => taddr, tdata => tdata);

    process
    begin
        clk <= not clk;
        wait for 5 ns;
    end process;

    process
    begin
        -- preload program into imem (one word per clock, cpu held in reset)
        imem_we <= '1';
        wait until rising_edge(clk);
        imem_waddr <= x"00"; imem_wdata <= x"00000093"; wait until rising_edge(clk);
        imem_waddr <= x"01"; imem_wdata <= x"00100113"; wait until rising_edge(clk);
        imem_waddr <= x"02"; imem_wdata <= x"00B00193"; wait until rising_edge(clk);
        imem_waddr <= x"03"; imem_wdata <= x"002080B3"; wait until rising_edge(clk);
        imem_waddr <= x"04"; imem_wdata <= x"00110113"; wait until rising_edge(clk);
        imem_waddr <= x"05"; imem_wdata <= x"FE314CE3"; wait until rising_edge(clk);
        imem_waddr <= x"06"; imem_wdata <= x"00102023"; wait until rising_edge(clk);
        imem_waddr <= x"07"; imem_wdata <= x"00100073"; wait until rising_edge(clk);
        wait until rising_edge(clk);
        imem_we <= '0';
        wait until rising_edge(clk);
        rst <= '0';

        wait until halted = '1';
        wait for 1 ns;
        taddr <= x"FF";
        wait for 1 ns;
        taddr <= x"00";
        wait for 1 ns;
        if tdata = x"00000037" then
            report "PASS: cpu sum(1..10) = 55 stored in dmem[0]";
        else
            report "FAIL: cpu sum dmem[0]=" & integer'image(to_integer(unsigned(tdata))) severity error;
            errors := errors + 1;
        end if;
        std.env.finish;
        wait;
    end process;
end architecture;
