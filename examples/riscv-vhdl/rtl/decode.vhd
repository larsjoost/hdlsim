-- RV32I instruction decoder. Combinational.
-- opcode = instr(6 downto 0); R-type ALU op selected from funct7/funct3.
library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;

entity decode is
    port (
        instr      : in  std_logic_vector(31 downto 0);
        reg_write  : out std_logic;
        alu_src    : out std_logic;   -- 1: ALU b = immediate
        alu_op     : out std_logic_vector(3 downto 0);
        mem_read   : out std_logic;
        mem_write  : out std_logic;
        mem_to_reg : out std_logic;   -- writeback from data memory
        branch     : out std_logic;
        jump       : out std_logic;   -- jal / jalr
        halt       : out std_logic    -- ebreak
    );
end entity;

architecture rtl of decode is
    -- ALU op codes (must match alu.vhd)
    constant OP_ADD   : std_logic_vector(3 downto 0) := "0000";
    constant OP_SUB   : std_logic_vector(3 downto 0) := "0001";
    constant OP_SLT   : std_logic_vector(3 downto 0) := "0010";
    constant OP_SLTU  : std_logic_vector(3 downto 0) := "0011";
    constant OP_XOR   : std_logic_vector(3 downto 0) := "0100";
    constant OP_SRL   : std_logic_vector(3 downto 0) := "0101";
    constant OP_OR    : std_logic_vector(3 downto 0) := "0110";
    constant OP_AND   : std_logic_vector(3 downto 0) := "0111";
    constant OP_SLL   : std_logic_vector(3 downto 0) := "1000";
    constant OP_SRA   : std_logic_vector(3 downto 0) := "1001";
    constant OP_LUI   : std_logic_vector(3 downto 0) := "1010";
    constant OP_AUIPC : std_logic_vector(3 downto 0) := "1011";

    signal opcode : std_logic_vector(6 downto 0);
    signal rw, asrc, mr, mw, mtr, br, jmp, hlt : std_logic;
    signal aop : std_logic_vector(3 downto 0);
begin
    opcode <= instr(6 downto 0);

    process(instr, opcode)
    begin
        -- defaults: nothing happens
        rw <= '0'; asrc <= '0'; mr <= '0'; mw <= '0';
        mtr <= '0'; br <= '0'; jmp <= '0'; hlt <= '0';
        aop <= OP_ADD;
        case opcode is
            when b"0110011" =>  -- R-type
                rw <= '1';
                case instr(14 downto 12) is
                    when "000" =>
                        if instr(30) = '1' then aop <= OP_SUB; else aop <= OP_ADD; end if;
                    when "001" => aop <= OP_SLL;
                    when "010" => aop <= OP_SLT;
                    when "011" => aop <= OP_SLTU;
                    when "100" => aop <= OP_XOR;
                    when "101" =>
                        if instr(30) = '1' then aop <= OP_SRA; else aop <= OP_SRL; end if;
                    when "110" => aop <= OP_OR;
                    when "111" => aop <= OP_AND;
                    when others => aop <= OP_ADD;
                end case;
            when b"0010011" =>  -- I-type arithmetic
                rw <= '1'; asrc <= '1';
                case instr(14 downto 12) is
                    when "000" => aop <= OP_ADD;
                    when "010" => aop <= OP_SLT;
                    when "011" => aop <= OP_SLTU;
                    when "100" => aop <= OP_XOR;
                    when "110" => aop <= OP_OR;
                    when "111" => aop <= OP_AND;
                    when "001" => aop <= OP_SLL;
                    when "101" =>
                        if instr(30) = '1' then aop <= OP_SRA; else aop <= OP_SRL; end if;
                    when others => aop <= OP_ADD;
                end case;
            when b"0000011" =>  -- loads (lw)
                rw <= '1'; asrc <= '1'; mr <= '1'; mtr <= '1';
                aop <= OP_ADD;
            when b"0100011" =>  -- stores (sw)
                asrc <= '1'; mw <= '1';
                aop <= OP_ADD;
            when b"1100011" =>  -- branches
                asrc <= '1'; br <= '1';
                aop <= OP_ADD;
            when b"1101111" =>  -- jal
                rw <= '1'; jmp <= '1';
                aop <= OP_ADD;
            when b"1100111" =>  -- jalr
                rw <= '1'; jmp <= '1'; asrc <= '1';
                aop <= OP_ADD;
            when b"0110111" =>  -- lui
                rw <= '1'; asrc <= '1';
                aop <= OP_LUI;
            when b"0010111" =>  -- auipc
                rw <= '1'; asrc <= '1';
                aop <= OP_AUIPC;
            when b"1110011" =>  -- system: ebreak halts
                if instr(31 downto 20) = x"001" then
                    hlt <= '1';
                end if;
            when others =>
                null;
        end case;
    end process;

    reg_write  <= rw;
    alu_src    <= asrc;
    alu_op     <= aop;
    mem_read   <= mr;
    mem_write  <= mw;
    mem_to_reg <= mtr;
    branch     <= br;
    jump       <= jmp;
    halt       <= hlt;
end architecture;
