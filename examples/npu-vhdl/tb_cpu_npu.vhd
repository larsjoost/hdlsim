-- Integration test: the RV32I cpu (library rv32i) drives the NPU (work.npu)
-- over its data bus to compute one 8x8 matvec y = W*x (an "LLM layer"):
-- weights w[k]=k and inputs x[j]=1 are preloaded into dmem, the cpu copies
-- them into the NPU via MMIO, starts it, polls STATUS, reads results back
-- into dmem[80..87]. Expected y[i] = 64*i + 28.
library ieee;
library rv32i;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;

entity tb_cpu_npu is
end entity;

architecture sim of tb_cpu_npu is
    signal clk : std_logic := '0';
    signal rst : std_logic := '1';
    signal halted : std_logic;
    signal imem_addr : unsigned(31 downto 0);
    signal imem_data : std_logic_vector(31 downto 0);
    signal dmem_addr : unsigned(31 downto 0);
    signal dmem_we : std_logic;
    signal dmem_wdata : unsigned(31 downto 0);
    signal cpu_rdata : std_logic_vector(31 downto 0);
    signal dmem_rdata : std_logic_vector(31 downto 0);
    signal imem_we : std_logic := '0';
    signal imem_waddr : unsigned(7 downto 0) := x"00";
    signal imem_wdata : std_logic_vector(31 downto 0) := x"00000000";
    -- dmem preload port
    signal pre_we : std_logic := '0';
    signal pre_waddr : unsigned(31 downto 0) := x"00000000";
    signal pre_wdata : unsigned(31 downto 0) := x"00000000";
    -- dmem effective port (preload mux)
    signal dmem_we_eff : std_logic;
    signal dmem_waddr_eff : unsigned(31 downto 0);
    signal dmem_wdata_eff : unsigned(31 downto 0);
    -- npu bus
    signal npu_sel : std_logic;
    signal npu_re : std_logic;
    signal npu_rdata : std_logic_vector(31 downto 0);
    -- dmem testbench read port
    signal taddr : unsigned(7 downto 0) := x"00";
    signal tdata : std_logic_vector(31 downto 0);
    signal errors : integer := 0;
begin
    u_cpu : entity rv32i.cpu
        port map (clk => clk, rst => rst, halted => halted,
                  imem_addr => imem_addr, imem_data => imem_data,
                  dmem_addr => dmem_addr, dmem_we => dmem_we,
                  dmem_wdata => dmem_wdata, dmem_rdata => cpu_rdata);
    u_imem : entity rv32i.imem
        port map (clk => clk, we => imem_we, waddr => imem_waddr, wdata => imem_wdata,
                  raddr => imem_addr, rdata => imem_data);
    u_dmem : entity rv32i.dmem
        port map (clk => clk, we => dmem_we_eff, waddr => dmem_waddr_eff, wdata => dmem_wdata_eff,
                  raddr => dmem_addr, rdata => dmem_rdata, taddr => taddr, tdata => tdata);
    u_npu : entity work.npu
        port map (clk => clk, addr => dmem_addr, we => dmem_we and npu_sel, re => npu_re,
                  wdata => dmem_wdata, rdata => npu_rdata);

    -- MMIO select: dmem addresses 0x1000..0x1FFF go to the NPU
    process(dmem_addr)
    begin
        if dmem_addr(31 downto 12) = x"001" then
            npu_sel <= '1';
        else
            npu_sel <= '0';
        end if;
    end process;

    -- NPU read strobe: MMIO select + not a write + the instruction is a load
    process(npu_sel, dmem_we, imem_data)
    begin
        if npu_sel = '1' and dmem_we = '0' and imem_data(6 downto 0) = b"0000011" then
            npu_re <= '1';
        else
            npu_re <= '0';
        end if;
    end process;

    -- cpu read-data mux: NPU or dmem
    process(npu_sel, npu_rdata, dmem_rdata)
    begin
        if npu_sel = '1' then
            cpu_rdata <= npu_rdata;
        else
            cpu_rdata <= dmem_rdata;
        end if;
    end process;

    -- dmem write port: preload wins, else cpu writes gated by not npu_sel
    process(pre_we, pre_waddr, pre_wdata, dmem_we, npu_sel, dmem_addr, dmem_wdata)
    begin
        if pre_we = '1' then
            dmem_we_eff <= '1';
            dmem_waddr_eff <= pre_waddr;
            dmem_wdata_eff <= pre_wdata;
        else
            dmem_we_eff <= dmem_we and not npu_sel;
            dmem_waddr_eff <= dmem_addr;
            dmem_wdata_eff <= dmem_wdata;
        end if;
    end process;

    process
    begin
        clk <= not clk;
        wait for 5 ns;
    end process;

    process
    begin
        -- preload program into imem
        imem_we <= '1';
        wait until rising_edge(clk);
        imem_waddr <= x"00"; imem_wdata <= x"000012B7"; wait until rising_edge(clk);
        imem_waddr <= x"01"; imem_wdata <= x"01028293"; wait until rising_edge(clk);
        imem_waddr <= x"02"; imem_wdata <= x"00001337"; wait until rising_edge(clk);
        imem_waddr <= x"03"; imem_wdata <= x"01430313"; wait until rising_edge(clk);
        imem_waddr <= x"04"; imem_wdata <= x"000013B7"; wait until rising_edge(clk);
        imem_waddr <= x"05"; imem_wdata <= x"01C38393"; wait until rising_edge(clk);
        imem_waddr <= x"06"; imem_wdata <= x"0003A023"; wait until rising_edge(clk);
        imem_waddr <= x"07"; imem_wdata <= x"00000413"; wait until rising_edge(clk);
        imem_waddr <= x"08"; imem_wdata <= x"04000493"; wait until rising_edge(clk);
        imem_waddr <= x"09"; imem_wdata <= x"00042E03"; wait until rising_edge(clk);
        imem_waddr <= x"0A"; imem_wdata <= x"01C2A023"; wait until rising_edge(clk);
        imem_waddr <= x"0B"; imem_wdata <= x"00440413"; wait until rising_edge(clk);
        imem_waddr <= x"0C"; imem_wdata <= x"FFF48493"; wait until rising_edge(clk);
        imem_waddr <= x"0D"; imem_wdata <= x"FE0498E3"; wait until rising_edge(clk);
        imem_waddr <= x"0E"; imem_wdata <= x"10000413"; wait until rising_edge(clk);
        imem_waddr <= x"0F"; imem_wdata <= x"00800493"; wait until rising_edge(clk);
        imem_waddr <= x"10"; imem_wdata <= x"00042E03"; wait until rising_edge(clk);
        imem_waddr <= x"11"; imem_wdata <= x"01C32023"; wait until rising_edge(clk);
        imem_waddr <= x"12"; imem_wdata <= x"00440413"; wait until rising_edge(clk);
        imem_waddr <= x"13"; imem_wdata <= x"FFF48493"; wait until rising_edge(clk);
        imem_waddr <= x"14"; imem_wdata <= x"FE0498E3"; wait until rising_edge(clk);
        imem_waddr <= x"15"; imem_wdata <= x"00100E13"; wait until rising_edge(clk);
        imem_waddr <= x"16"; imem_wdata <= x"00001EB7"; wait until rising_edge(clk);
        imem_waddr <= x"17"; imem_wdata <= x"01CEA023"; wait until rising_edge(clk);
        imem_waddr <= x"18"; imem_wdata <= x"004E8E93"; wait until rising_edge(clk);
        imem_waddr <= x"19"; imem_wdata <= x"000EAE03"; wait until rising_edge(clk);
        imem_waddr <= x"1A"; imem_wdata <= x"002E7E13"; wait until rising_edge(clk);
        imem_waddr <= x"1B"; imem_wdata <= x"FE0E0CE3"; wait until rising_edge(clk);
        imem_waddr <= x"1C"; imem_wdata <= x"00001EB7"; wait until rising_edge(clk);
        imem_waddr <= x"1D"; imem_wdata <= x"018E8E93"; wait until rising_edge(clk);
        imem_waddr <= x"1E"; imem_wdata <= x"14000413"; wait until rising_edge(clk);
        imem_waddr <= x"1F"; imem_wdata <= x"00800493"; wait until rising_edge(clk);
        imem_waddr <= x"20"; imem_wdata <= x"000EAE03"; wait until rising_edge(clk);
        imem_waddr <= x"21"; imem_wdata <= x"01C42023"; wait until rising_edge(clk);
        imem_waddr <= x"22"; imem_wdata <= x"00440413"; wait until rising_edge(clk);
        imem_waddr <= x"23"; imem_wdata <= x"FFF48493"; wait until rising_edge(clk);
        imem_waddr <= x"24"; imem_wdata <= x"FE0498E3"; wait until rising_edge(clk);
        imem_waddr <= x"25"; imem_wdata <= x"00100073"; wait until rising_edge(clk);
        wait until rising_edge(clk);
        imem_we <= '0';
        -- preload dmem: weights w[k]=k at words 0..63, inputs x[j]=1 at 64..71
        pre_we <= '1';
        wait until rising_edge(clk);
        pre_waddr <= to_unsigned(0, 32); pre_wdata <= x"00000000"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(4, 32); pre_wdata <= x"00000001"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(8, 32); pre_wdata <= x"00000002"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(12, 32); pre_wdata <= x"00000003"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(16, 32); pre_wdata <= x"00000004"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(20, 32); pre_wdata <= x"00000005"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(24, 32); pre_wdata <= x"00000006"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(28, 32); pre_wdata <= x"00000007"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(32, 32); pre_wdata <= x"00000008"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(36, 32); pre_wdata <= x"00000009"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(40, 32); pre_wdata <= x"0000000A"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(44, 32); pre_wdata <= x"0000000B"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(48, 32); pre_wdata <= x"0000000C"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(52, 32); pre_wdata <= x"0000000D"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(56, 32); pre_wdata <= x"0000000E"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(60, 32); pre_wdata <= x"0000000F"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(64, 32); pre_wdata <= x"00000010"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(68, 32); pre_wdata <= x"00000011"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(72, 32); pre_wdata <= x"00000012"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(76, 32); pre_wdata <= x"00000013"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(80, 32); pre_wdata <= x"00000014"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(84, 32); pre_wdata <= x"00000015"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(88, 32); pre_wdata <= x"00000016"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(92, 32); pre_wdata <= x"00000017"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(96, 32); pre_wdata <= x"00000018"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(100, 32); pre_wdata <= x"00000019"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(104, 32); pre_wdata <= x"0000001A"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(108, 32); pre_wdata <= x"0000001B"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(112, 32); pre_wdata <= x"0000001C"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(116, 32); pre_wdata <= x"0000001D"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(120, 32); pre_wdata <= x"0000001E"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(124, 32); pre_wdata <= x"0000001F"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(128, 32); pre_wdata <= x"00000020"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(132, 32); pre_wdata <= x"00000021"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(136, 32); pre_wdata <= x"00000022"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(140, 32); pre_wdata <= x"00000023"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(144, 32); pre_wdata <= x"00000024"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(148, 32); pre_wdata <= x"00000025"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(152, 32); pre_wdata <= x"00000026"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(156, 32); pre_wdata <= x"00000027"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(160, 32); pre_wdata <= x"00000028"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(164, 32); pre_wdata <= x"00000029"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(168, 32); pre_wdata <= x"0000002A"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(172, 32); pre_wdata <= x"0000002B"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(176, 32); pre_wdata <= x"0000002C"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(180, 32); pre_wdata <= x"0000002D"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(184, 32); pre_wdata <= x"0000002E"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(188, 32); pre_wdata <= x"0000002F"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(192, 32); pre_wdata <= x"00000030"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(196, 32); pre_wdata <= x"00000031"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(200, 32); pre_wdata <= x"00000032"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(204, 32); pre_wdata <= x"00000033"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(208, 32); pre_wdata <= x"00000034"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(212, 32); pre_wdata <= x"00000035"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(216, 32); pre_wdata <= x"00000036"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(220, 32); pre_wdata <= x"00000037"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(224, 32); pre_wdata <= x"00000038"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(228, 32); pre_wdata <= x"00000039"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(232, 32); pre_wdata <= x"0000003A"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(236, 32); pre_wdata <= x"0000003B"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(240, 32); pre_wdata <= x"0000003C"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(244, 32); pre_wdata <= x"0000003D"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(248, 32); pre_wdata <= x"0000003E"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(252, 32); pre_wdata <= x"0000003F"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(256, 32); pre_wdata <= x"00000001"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(260, 32); pre_wdata <= x"00000001"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(264, 32); pre_wdata <= x"00000001"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(268, 32); pre_wdata <= x"00000001"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(272, 32); pre_wdata <= x"00000001"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(276, 32); pre_wdata <= x"00000001"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(280, 32); pre_wdata <= x"00000001"; wait until rising_edge(clk);
        pre_waddr <= to_unsigned(284, 32); pre_wdata <= x"00000001"; wait until rising_edge(clk);
        wait until rising_edge(clk);
        pre_we <= '0';
        wait until rising_edge(clk);
        rst <= '0';

        wait until halted = '1';
        wait for 1 ns;
        taddr <= to_unsigned(80, 8);
        wait for 1 ns;
        if tdata = x"0000001C" then
            report "PASS: npu y[0]=28";
        else
            report "FAIL: npu y[0]=" & integer'image(to_integer(unsigned(tdata))) severity error;
            errors := errors + 1;
        end if;
        taddr <= to_unsigned(81, 8);
        wait for 1 ns;
        if tdata = x"0000005C" then
            report "PASS: npu y[1]=92";
        else
            report "FAIL: npu y[1]=" & integer'image(to_integer(unsigned(tdata))) severity error;
            errors := errors + 1;
        end if;
        taddr <= to_unsigned(82, 8);
        wait for 1 ns;
        if tdata = x"0000009C" then
            report "PASS: npu y[2]=156";
        else
            report "FAIL: npu y[2]=" & integer'image(to_integer(unsigned(tdata))) severity error;
            errors := errors + 1;
        end if;
        taddr <= to_unsigned(83, 8);
        wait for 1 ns;
        if tdata = x"000000DC" then
            report "PASS: npu y[3]=220";
        else
            report "FAIL: npu y[3]=" & integer'image(to_integer(unsigned(tdata))) severity error;
            errors := errors + 1;
        end if;
        taddr <= to_unsigned(84, 8);
        wait for 1 ns;
        if tdata = x"0000011C" then
            report "PASS: npu y[4]=284";
        else
            report "FAIL: npu y[4]=" & integer'image(to_integer(unsigned(tdata))) severity error;
            errors := errors + 1;
        end if;
        taddr <= to_unsigned(85, 8);
        wait for 1 ns;
        if tdata = x"0000015C" then
            report "PASS: npu y[5]=348";
        else
            report "FAIL: npu y[5]=" & integer'image(to_integer(unsigned(tdata))) severity error;
            errors := errors + 1;
        end if;
        taddr <= to_unsigned(86, 8);
        wait for 1 ns;
        if tdata = x"0000019C" then
            report "PASS: npu y[6]=412";
        else
            report "FAIL: npu y[6]=" & integer'image(to_integer(unsigned(tdata))) severity error;
            errors := errors + 1;
        end if;
        taddr <= to_unsigned(87, 8);
        wait for 1 ns;
        if tdata = x"000001DC" then
            report "PASS: npu y[7]=476";
        else
            report "FAIL: npu y[7]=" & integer'image(to_integer(unsigned(tdata))) severity error;
            errors := errors + 1;
        end if;
        if errors = 0 then
            report "PASS: cpu+npu matvec (8x8 LLM layer) all 8 results correct";
        else
            report "FAIL: cpu+npu — " & integer'image(errors) & " errors" severity error;
        end if;
        std.env.finish;
        wait;
    end process;
end architecture;
