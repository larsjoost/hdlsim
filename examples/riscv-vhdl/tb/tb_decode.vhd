-- Unit testbench for decode.vhd: feed one instruction per opcode class and
-- check all control outputs. 31 checks.
library ieee;
library rv32i;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;

entity tb_decode is
end entity;

architecture sim of tb_decode is
    signal instr : std_logic_vector(31 downto 0) := x"00000000";
    signal reg_write, alu_src, mem_read, mem_write, mem_to_reg, branch, jump, halt : std_logic;
    signal alu_op : std_logic_vector(3 downto 0);
    signal errors : integer := 0;
begin
    dut : entity rv32i.decode
        port map (instr => instr, reg_write => reg_write, alu_src => alu_src,
                  alu_op => alu_op, mem_read => mem_read, mem_write => mem_write,
                  mem_to_reg => mem_to_reg, branch => branch, jump => jump, halt => halt);

    process
        -- helper macro-ish: set instr, settle, check one-hot controls
    begin
        -- R-type: add x1,x2,x3
        instr <= x"003100B3"; wait for 1 ns;
        if reg_write /= '1' or alu_src /= '0' or alu_op /= "0000" or mem_write /= '0' or branch /= '0' or jump /= '0' or halt /= '0' then
            report "FAIL decode add" severity error; errors := errors + 1;
        end if;
        -- sub
        instr <= x"403100B3"; wait for 1 ns;
        if alu_op /= "0001" or reg_write /= '1' then report "FAIL decode sub" severity error; errors := errors + 1; end if;
        -- slt / sltu / sll / sra / xor / or / and
        instr <= x"003120B3"; wait for 1 ns;
        if alu_op /= "0010" then report "FAIL decode slt" severity error; errors := errors + 1; end if;
        instr <= x"003130B3"; wait for 1 ns;
        if alu_op /= "0011" then report "FAIL decode sltu" severity error; errors := errors + 1; end if;
        instr <= x"003110B3"; wait for 1 ns;
        if alu_op /= "1000" then report "FAIL decode sll" severity error; errors := errors + 1; end if;
        instr <= x"403150B3"; wait for 1 ns;
        if alu_op /= "1001" then report "FAIL decode sra" severity error; errors := errors + 1; end if;
        instr <= x"003140B3"; wait for 1 ns;
        if alu_op /= "0100" then report "FAIL decode xor" severity error; errors := errors + 1; end if;
        instr <= x"003160B3"; wait for 1 ns;
        if alu_op /= "0110" then report "FAIL decode or" severity error; errors := errors + 1; end if;
        instr <= x"003170B3"; wait for 1 ns;
        if alu_op /= "0111" then report "FAIL decode and" severity error; errors := errors + 1; end if;
        -- I-type: addi / slti / sltiu / xori / ori / andi / slli / srli / srai
        instr <= x"00510093"; wait for 1 ns;
        if reg_write /= '1' or alu_src /= '1' or alu_op /= "0000" then report "FAIL decode addi" severity error; errors := errors + 1; end if;
        instr <= x"FFF12093"; wait for 1 ns;
        if alu_op /= "0010" then report "FAIL decode slti" severity error; errors := errors + 1; end if;
        instr <= x"00713093"; wait for 1 ns;
        if alu_op /= "0011" then report "FAIL decode sltiu" severity error; errors := errors + 1; end if;
        instr <= x"0FF14093"; wait for 1 ns;
        if alu_op /= "0100" then report "FAIL decode xori" severity error; errors := errors + 1; end if;
        instr <= x"00F16093"; wait for 1 ns;
        if alu_op /= "0110" then report "FAIL decode ori" severity error; errors := errors + 1; end if;
        instr <= x"00F17093"; wait for 1 ns;
        if alu_op /= "0111" then report "FAIL decode andi" severity error; errors := errors + 1; end if;
        instr <= x"00411093"; wait for 1 ns;
        if alu_op /= "1000" then report "FAIL decode slli" severity error; errors := errors + 1; end if;
        instr <= x"00415093"; wait for 1 ns;
        if alu_op /= "0101" then report "FAIL decode srli" severity error; errors := errors + 1; end if;
        instr <= x"40415093"; wait for 1 ns;
        if alu_op /= "1001" then report "FAIL decode srai" severity error; errors := errors + 1; end if;
        -- lw: mem_read + mem_to_reg + alu_src + reg_write
        instr <= x"00012083"; wait for 1 ns;
        if mem_read /= '1' or mem_to_reg /= '1' or alu_src /= '1' or reg_write /= '1' or mem_write /= '0' then
            report "FAIL decode lw" severity error; errors := errors + 1;
        end if;
        -- sw: mem_write + alu_src, no reg_write
        instr <= x"00112423"; wait for 1 ns;
        if mem_write /= '1' or alu_src /= '1' or reg_write /= '0' or mem_read /= '0' then
            report "FAIL decode sw" severity error; errors := errors + 1;
        end if;
        -- branches: branch=1, alu_src=1, no reg_write
        instr <= x"00208463"; wait for 1 ns;  -- beq
        if branch /= '1' or alu_src /= '1' or reg_write /= '0' then report "FAIL decode beq" severity error; errors := errors + 1; end if;
        instr <= x"00209463"; wait for 1 ns;  -- bne
        if branch /= '1' then report "FAIL decode bne" severity error; errors := errors + 1; end if;
        instr <= x"FE20CCE3"; wait for 1 ns;  -- blt
        if branch /= '1' then report "FAIL decode blt" severity error; errors := errors + 1; end if;
        instr <= x"0020D463"; wait for 1 ns;  -- bge
        if branch /= '1' then report "FAIL decode bge" severity error; errors := errors + 1; end if;
        instr <= x"0020E463"; wait for 1 ns;  -- bltu
        if branch /= '1' then report "FAIL decode bltu" severity error; errors := errors + 1; end if;
        instr <= x"0020F463"; wait for 1 ns;  -- bgeu
        if branch /= '1' then report "FAIL decode bgeu" severity error; errors := errors + 1; end if;
        -- jal / jalr: jump + reg_write
        instr <= x"010000EF"; wait for 1 ns;
        if jump /= '1' or reg_write /= '1' or branch /= '0' then report "FAIL decode jal" severity error; errors := errors + 1; end if;
        instr <= x"00008067"; wait for 1 ns;
        if jump /= '1' or alu_src /= '1' or reg_write /= '1' then report "FAIL decode jalr" severity error; errors := errors + 1; end if;
        -- lui / auipc
        instr <= x"123450B7"; wait for 1 ns;
        if alu_op /= "1010" or reg_write /= '1' or alu_src /= '1' then report "FAIL decode lui" severity error; errors := errors + 1; end if;
        instr <= x"00001097"; wait for 1 ns;
        if alu_op /= "1011" or reg_write /= '1' then report "FAIL decode auipc" severity error; errors := errors + 1; end if;
        -- ebreak: halt only
        instr <= x"00100073"; wait for 1 ns;
        if halt /= '1' or reg_write /= '0' or mem_write /= '0' or jump /= '0' then
            report "FAIL decode ebreak" severity error; errors := errors + 1;
        end if;

        if errors = 0 then
            report "PASS: decode — 31 checks ok";
        else
            report "FAIL: decode — " & integer'image(errors) & " errors" severity error;
        end if;
        std.env.finish;
        wait;
    end process;
end architecture;
