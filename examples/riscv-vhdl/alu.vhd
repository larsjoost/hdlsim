-- ALU for the RV32I example CPU. Combinational.
-- op encoding: 0000 ADD, 0001 SUB, 0010 SLT (signed), 0011 SLTU,
--              0100 XOR, 0101 SRL, 0110 OR, 0111 AND, 1000 SLL, 1001 SRA,
--              1010 LUI (result = b), 1011 AUIPC (result = a + b, a = pc).
library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;

entity alu is
    port (
        a      : in  unsigned(31 downto 0);
        b      : in  unsigned(31 downto 0);
        op     : in  std_logic_vector(3 downto 0);
        result : out unsigned(31 downto 0);
        zero   : out std_logic
    );
end entity;

architecture rtl of alu is
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
    signal res : unsigned(31 downto 0) := x"00000000";
begin
    process(a, b, op)
    begin
        case op is
            when OP_ADD => res <= a + b;
            when OP_SUB => res <= a - b;
            when OP_SLT =>
                if signed(a) < signed(b) then
                    res <= x"00000001";
                else
                    res <= x"00000000";
                end if;
            when OP_SLTU =>
                if a < b then
                    res <= x"00000001";
                else
                    res <= x"00000000";
                end if;
            when OP_XOR => res <= a xor b;
            when OP_SRL => res <= a srl to_integer(b(4 downto 0));
            when OP_OR  => res <= a or b;
            when OP_AND => res <= a and b;
            when OP_SLL => res <= a sll to_integer(b(4 downto 0));
            when OP_SRA => res <= unsigned(signed(a) sra to_integer(b(4 downto 0)));
            when OP_LUI   => res <= b;
            when OP_AUIPC => res <= a + b;
            when others   => res <= x"00000000";
        end case;
    end process;

    result <= res;

    process(res)
    begin
        if res = x"00000000" then
            zero <= '1';
        else
            zero <= '0';
        end if;
    end process;
end architecture;
