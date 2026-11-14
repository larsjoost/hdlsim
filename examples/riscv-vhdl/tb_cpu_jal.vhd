-- Integration test: subroutine call/return via jal/jalr.
-- double(21) = 42 stored in dmem[3].
--   0: addi x1, x0, 21    x1 = 21
--   1: jal  x4, +8        call sub at word 3
--   2: ebreak
--   3: slli x1, x1, 1     sub: x1 <<= 1
--   4: sw   x1, 12(x0)    dmem[3] = x1
--   5: jalr x0, x4, 0     return
library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;

entity tb_cpu_jal is
end entity;

architecture sim of tb_cpu_jal is
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
        imem_waddr <= x"00"; imem_wdata <= x"01500093"; wait until rising_edge(clk);
        imem_waddr <= x"01"; imem_wdata <= x"0080026F"; wait until rising_edge(clk);
        imem_waddr <= x"02"; imem_wdata <= x"00100073"; wait until rising_edge(clk);
        imem_waddr <= x"03"; imem_wdata <= x"00109093"; wait until rising_edge(clk);
        imem_waddr <= x"04"; imem_wdata <= x"00102623"; wait until rising_edge(clk);
        imem_waddr <= x"05"; imem_wdata <= x"00020067"; wait until rising_edge(clk);
        wait until rising_edge(clk);
        imem_we <= '0';
        wait until rising_edge(clk);
        rst <= '0';

        wait until halted = '1';
        wait for 1 ns;
        taddr <= x"FF";
        wait for 1 ns;
        taddr <= x"03";
        wait for 1 ns;
        if tdata = x"0000002A" then
            report "PASS: cpu jal/jalr subroutine double(21)=42 in dmem[3]";
        else
            report "FAIL: cpu jal dmem[3]=" & integer'image(to_integer(unsigned(tdata))) severity error;
            errors := errors + 1;
        end if;
        std.env.finish;
        wait;
    end process;
end architecture;
