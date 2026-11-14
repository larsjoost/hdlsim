-- Unit testbench for alu.vhd: drives every ALU op with several operand
-- pairs (including signed edge cases) and checks result + zero flag.
library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;

entity tb_alu is
end entity;

architecture sim of tb_alu is
    signal a, b, result : unsigned(31 downto 0);
    signal op : std_logic_vector(3 downto 0);
    signal zero : std_logic;
    signal errors : integer := 0;
begin
    dut : entity work.alu
        port map (a => a, b => b, op => op, result => result, zero => zero);

    process
    begin
        -- ADD
        a <= x"00000005"; b <= x"00000003"; op <= "0000"; wait for 1 ns;
        if result /= x"00000008" then report "FAIL alu add 5+3" severity error; errors := errors + 1; end if;
        a <= x"FFFFFFFF"; b <= x"00000001"; wait for 1 ns;
        if result /= x"00000000" then report "FAIL alu add wrap" severity error; errors := errors + 1; end if;
        a <= x"7FFFFFFF"; b <= x"00000001"; wait for 1 ns;
        if result /= x"80000000" then report "FAIL alu add overflow" severity error; errors := errors + 1; end if;
        -- SUB
        op <= "0001";
        a <= x"00000005"; b <= x"00000003"; wait for 1 ns;
        if result /= x"00000002" then report "FAIL alu sub 5-3" severity error; errors := errors + 1; end if;
        a <= x"00000003"; b <= x"00000005"; wait for 1 ns;
        if result /= x"FFFFFFFE" then report "FAIL alu sub 3-5" severity error; errors := errors + 1; end if;
        a <= x"00000000"; b <= x"00000001"; wait for 1 ns;
        if result /= x"FFFFFFFF" then report "FAIL alu sub 0-1" severity error; errors := errors + 1; end if;
        if zero = '1' then report "FAIL alu zero flag on sub" severity error; errors := errors + 1; end if;
        -- SLT (signed)
        op <= "0010";
        a <= x"FFFFFFFF"; b <= x"00000001"; wait for 1 ns;  -- -1 < 1
        if result /= x"00000001" then report "FAIL alu slt -1<1" severity error; errors := errors + 1; end if;
        a <= x"00000001"; b <= x"FFFFFFFF"; wait for 1 ns;  -- 1 < -1 false
        if result /= x"00000000" then report "FAIL alu slt 1<-1" severity error; errors := errors + 1; end if;
        a <= x"FFFFFFFB"; b <= x"FFFFFFFD"; wait for 1 ns;  -- -5 < -3
        if result /= x"00000001" then report "FAIL alu slt -5<-3" severity error; errors := errors + 1; end if;
        -- SLTU (unsigned)
        op <= "0011";
        a <= x"FFFFFFFF"; b <= x"00000001"; wait for 1 ns;
        if result /= x"00000000" then report "FAIL alu sltu max<1" severity error; errors := errors + 1; end if;
        a <= x"00000001"; b <= x"FFFFFFFF"; wait for 1 ns;
        if result /= x"00000001" then report "FAIL alu sltu 1<max" severity error; errors := errors + 1; end if;
        -- XOR / OR / AND
        op <= "0100";
        a <= x"F0F0F0F0"; b <= x"FF00FF00"; wait for 1 ns;
        if result /= x"0FF00FF0" then report "FAIL alu xor" severity error; errors := errors + 1; end if;
        op <= "0110";
        a <= x"F0F00000"; b <= x"0000F00F"; wait for 1 ns;
        if result /= x"F0F0F00F" then report "FAIL alu or" severity error; errors := errors + 1; end if;
        op <= "0111";
        a <= x"FF00FF00"; b <= x"0F0F0F0F"; wait for 1 ns;
        if result /= x"0F000F00" then report "FAIL alu and" severity error; errors := errors + 1; end if;
        -- SLL / SRL / SRA
        op <= "1000";
        a <= x"00000001"; b <= x"00000004"; wait for 1 ns;
        if result /= x"00000010" then report "FAIL alu sll 1<<4" severity error; errors := errors + 1; end if;
        a <= x"80000000"; b <= x"00000001"; wait for 1 ns;
        if result /= x"00000000" then report "FAIL alu sll wrap" severity error; errors := errors + 1; end if;
        op <= "0101";
        a <= x"80000000"; b <= x"00000004"; wait for 1 ns;
        if result /= x"08000000" then report "FAIL alu srl" severity error; errors := errors + 1; end if;
        a <= x"FFFFFFFF"; b <= x"0000001F"; wait for 1 ns;
        if result /= x"00000001" then report "FAIL alu srl 31" severity error; errors := errors + 1; end if;
        op <= "1001";
        a <= x"80000000"; b <= x"00000004"; wait for 1 ns;
        if result /= x"F8000000" then report "FAIL alu sra neg" severity error; errors := errors + 1; end if;
        a <= x"7FFFFFFF"; b <= x"00000004"; wait for 1 ns;
        if result /= x"07FFFFFF" then report "FAIL alu sra pos" severity error; errors := errors + 1; end if;
        -- LUI: result = b
        op <= "1010";
        a <= x"00000000"; b <= x"12345000"; wait for 1 ns;
        if result /= x"12345000" then report "FAIL alu lui" severity error; errors := errors + 1; end if;
        -- AUIPC: a + b
        op <= "1011";
        a <= x"00000100"; b <= x"00000020"; wait for 1 ns;
        if result /= x"00000120" then report "FAIL alu auipc" severity error; errors := errors + 1; end if;
        -- zero flag: 5 - 5 = 0
        op <= "0001";
        a <= x"00000005"; b <= x"00000005"; wait for 1 ns;
        if zero /= '1' then report "FAIL alu zero flag" severity error; errors := errors + 1; end if;

        if errors = 0 then
            report "PASS: alu — 20 checks ok";
        else
            report "FAIL: alu — " & integer'image(errors) & " errors" severity error;
        end if;
        std.env.finish;
        wait;
    end process;
end architecture;
