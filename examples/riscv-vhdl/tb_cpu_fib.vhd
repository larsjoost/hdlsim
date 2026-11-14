-- Integration test: fibonacci. Computes fib(10)=55 into dmem[1], loads it
-- back into x5 with lw and stores it into dmem[2]. Checks both words.
--   0: addi x1, x0, 0     a = 0
--   1: addi x2, x0, 1     b = 1
--   2: addi x3, x0, 10    n = 10
--   3: add  x4, x1, x2    loop: tmp = a+b
--   4: add  x1, x2, x0    a = b
--   5: add  x2, x4, x0    b = tmp
--   6: addi x3, x3, -1    n--
--   7: bne  x3, x0, -16   if n /= 0 goto loop
--   8: sw   x1, 4(x0)     dmem[1] = a
--   9: lw   x5, 4(x0)     x5 = dmem[1]
--  10: sw   x5, 8(x0)     dmem[2] = x5
--  11: ebreak
library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;

entity tb_cpu_fib is
end entity;

architecture sim of tb_cpu_fib is
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
    u_cpu : entity work.cpu
        port map (clk => clk, rst => rst, halted => halted,
                  imem_addr => imem_addr, imem_data => imem_data,
                  dmem_addr => dmem_addr, dmem_we => dmem_we,
                  dmem_wdata => dmem_wdata, dmem_rdata => dmem_rdata);
    u_imem : entity work.imem
        port map (clk => clk, we => imem_we, waddr => imem_waddr, wdata => imem_wdata,
                  raddr => imem_addr, rdata => imem_data);
    u_dmem : entity work.dmem
        port map (clk => clk, we => dmem_we, waddr => dmem_addr, wdata => dmem_wdata,
                  raddr => dmem_addr, rdata => dmem_rdata, taddr => taddr, tdata => tdata);

    process
    begin
        clk <= not clk;
        wait for 5 ns;
    end process;

    process
    begin
        imem_we <= '1';
        wait until rising_edge(clk);
        imem_waddr <= x"00"; imem_wdata <= x"00000093"; wait until rising_edge(clk);
        imem_waddr <= x"01"; imem_wdata <= x"00100113"; wait until rising_edge(clk);
        imem_waddr <= x"02"; imem_wdata <= x"00A00193"; wait until rising_edge(clk);
        imem_waddr <= x"03"; imem_wdata <= x"00208233"; wait until rising_edge(clk);
        imem_waddr <= x"04"; imem_wdata <= x"000100B3"; wait until rising_edge(clk);
        imem_waddr <= x"05"; imem_wdata <= x"00020133"; wait until rising_edge(clk);
        imem_waddr <= x"06"; imem_wdata <= x"FFF18193"; wait until rising_edge(clk);
        imem_waddr <= x"07"; imem_wdata <= x"FE0198E3"; wait until rising_edge(clk);
        imem_waddr <= x"08"; imem_wdata <= x"00102223"; wait until rising_edge(clk);
        imem_waddr <= x"09"; imem_wdata <= x"00402283"; wait until rising_edge(clk);
        imem_waddr <= x"0A"; imem_wdata <= x"00502423"; wait until rising_edge(clk);
        imem_waddr <= x"0B"; imem_wdata <= x"00100073"; wait until rising_edge(clk);
        wait until rising_edge(clk);
        imem_we <= '0';
        wait until rising_edge(clk);
        rst <= '0';

        wait until halted = '1';
        wait for 1 ns;
        taddr <= x"01";
        wait for 1 ns;
        if tdata /= x"00000037" then
            report "FAIL: fib dmem[1]=" & integer'image(to_integer(unsigned(tdata))) severity error;
            errors := errors + 1;
        end if;
        taddr <= x"02";
        wait for 1 ns;
        if tdata /= x"00000037" then
            report "FAIL: fib lw/sw dmem[2]=" & integer'image(to_integer(unsigned(tdata))) severity error;
            errors := errors + 1;
        end if;
        if errors = 0 then
            report "PASS: cpu fib(10)=55 stored, loaded back, stored again";
        end if;
        std.env.finish;
        wait;
    end process;
end architecture;
