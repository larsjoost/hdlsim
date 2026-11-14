-- Single-cycle RV32I CPU (subset: all base integer instructions used by the
-- example programs — R/I/load/store/branch/jal/jalr/lui/auipc + ebreak halt).
-- Harvard architecture: separate instruction and data memory ports.
library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;

entity cpu is
    port (
        clk        : in  std_logic;
        rst        : in  std_logic;
        halted     : out std_logic;
        imem_addr  : out unsigned(31 downto 0);
        imem_data  : in  std_logic_vector(31 downto 0);
        dmem_addr  : out unsigned(31 downto 0);
        dmem_we    : out std_logic;
        dmem_wdata : out unsigned(31 downto 0);
        dmem_rdata : in  std_logic_vector(31 downto 0)
    );
end entity;

architecture rtl of cpu is
    signal pc : unsigned(31 downto 0) := x"00000000";
    signal instr : std_logic_vector(31 downto 0);

    -- decoder outputs
    signal reg_write  : std_logic;
    signal alu_src    : std_logic;
    signal alu_op     : std_logic_vector(3 downto 0);
    signal mem_read   : std_logic;
    signal mem_write  : std_logic;
    signal mem_to_reg : std_logic;
    signal branch     : std_logic;
    signal jump       : std_logic;
    signal halt       : std_logic;

    signal rd1, rd2 : unsigned(31 downto 0);
    signal imm_i, imm_s, imm_b, imm_u, imm_j : unsigned(31 downto 0);
    signal alu_a, alu_b, alu_result : unsigned(31 downto 0);
    signal wb : unsigned(31 downto 0);
    signal branch_taken : std_logic;
    signal next_pc : unsigned(31 downto 0);
    signal halted_s : std_logic := '0';
    signal alu_zero : std_logic;  -- unused ALU flag
begin
    imem_addr <= pc;
    instr <= imem_data;

    u_decode : entity work.decode
        port map (instr => instr, reg_write => reg_write, alu_src => alu_src,
                  alu_op => alu_op, mem_read => mem_read, mem_write => mem_write,
                  mem_to_reg => mem_to_reg, branch => branch, jump => jump,
                  halt => halt);

    u_regfile : entity work.regfile
        port map (clk => clk, we => reg_write and not halted_s, ra1 => unsigned(instr(19 downto 15)),
                  ra2 => unsigned(instr(24 downto 20)), wa => unsigned(instr(11 downto 7)),
                  wd => wb, rd1 => rd1, rd2 => rd2);

    u_alu : entity work.alu
        port map (a => alu_a, b => alu_b, op => alu_op, result => alu_result, zero => alu_zero);

    -- immediates (combinational, from the raw instruction)
    process(instr)
    begin
        imm_i <= unsigned(resize(signed(instr(31 downto 20)), 32));
        imm_s <= unsigned(resize(signed(instr(31 downto 25) & instr(11 downto 7)), 32));
        imm_b <= unsigned(resize(signed(instr(31) & instr(7) & instr(30 downto 25) & instr(11 downto 8) & '0'), 32));
        imm_u <= instr(31 downto 12) & x"000";
        imm_j <= unsigned(resize(signed(instr(31) & instr(19 downto 12) & instr(20) & instr(30 downto 21) & '0'), 32));
    end process;

    -- ALU input muxes
    process(rd1, rd2, alu_src, imm_i, imm_s, imm_u, instr)
    begin
        if instr(6 downto 0) = b"0010111" then
            alu_a <= pc;           -- auipc: pc + imm
        else
            alu_a <= rd1;
        end if;
        if alu_src = '1' then
            if instr(6 downto 0) = b"0100011" then
                alu_b <= imm_s;    -- stores use the S-type immediate
            elsif instr(6 downto 0) = b"0110111" or instr(6 downto 0) = b"0010111" then
                alu_b <= imm_u;    -- lui/auipc use the U-type immediate
            else
                alu_b <= imm_i;
            end if;
        else
            alu_b <= rd2;
        end if;
    end process;

    -- writeback mux
    process(mem_to_reg, jump, alu_result, dmem_rdata, pc)
    begin
        if mem_to_reg = '1' then
            wb <= unsigned(dmem_rdata);
        elsif jump = '1' then
            wb <= pc + 4;          -- jal/jalr: link = pc + 4
        else
            wb <= alu_result;
        end if;
    end process;

    -- branch decision
    process(branch, instr, rd1, rd2)
    begin
        branch_taken <= '0';
        if branch = '1' then
            case instr(14 downto 12) is
                when "000" => if rd1 = rd2 then branch_taken <= '1'; end if;              -- beq
                when "001" => if rd1 /= rd2 then branch_taken <= '1'; end if;             -- bne
                when "100" => if signed(rd1) < signed(rd2) then branch_taken <= '1'; end if;  -- blt
                when "101" => if signed(rd1) >= signed(rd2) then branch_taken <= '1'; end if; -- bge
                when "110" => if rd1 < rd2 then branch_taken <= '1'; end if;              -- bltu
                when "111" => if rd1 >= rd2 then branch_taken <= '1'; end if;             -- bgeu
                when others => null;
            end case;
        end if;
    end process;

    -- next pc
    process(pc, halt, halted_s, branch, branch_taken, jump, imm_b, imm_j, rd1, imm_i, instr)
    begin
        if halted_s = '1' or halt = '1' then
            next_pc <= pc;
        elsif branch = '1' and branch_taken = '1' then
            next_pc <= pc + imm_b;
        elsif jump = '1' then
            if instr(6 downto 0) = b"1100111" then
                next_pc <= (rd1 + imm_i) and x"FFFFFFFE";  -- jalr: (rs1+imm) & ~1
            else
                next_pc <= pc + imm_j;                       -- jal
            end if;
        else
            next_pc <= pc + 4;
        end if;
    end process;

    -- data memory interface (word addressed)
    dmem_addr  <= alu_result;
    dmem_we    <= mem_write and not halted_s;
    dmem_wdata <= rd2;

    -- pc register
    process(clk)
    begin
        if rising_edge(clk) then
            if rst = '1' then
                pc <= x"00000000";
                halted_s <= '0';
            else
                pc <= next_pc;
                if halt = '1' then
                    halted_s <= '1';
                end if;
            end if;
        end if;
    end process;

    halted <= halted_s;
end architecture;
