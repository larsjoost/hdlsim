-- Instruction memory: 256 words, asynchronous read, synchronous write
-- (the testbench preloads the program through the write port before reset).
library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;

entity imem is
    port (
        clk   : in  std_logic;
        we    : in  std_logic;
        waddr : in  unsigned(7 downto 0);           -- word index
        wdata : in  std_logic_vector(31 downto 0);
        raddr : in  unsigned(31 downto 0);          -- byte address from cpu
        rdata : out std_logic_vector(31 downto 0)
    );
end entity;

architecture rtl of imem is
    type mem_t is array (0 to 255) of std_logic_vector(31 downto 0);
    signal mem : mem_t := (others => x"00000000");
begin
    process(clk)
    begin
        if rising_edge(clk) then
            if we = '1' then
                mem(to_integer(waddr)) <= wdata;
            end if;
        end if;
    end process;

    rdata <= mem(to_integer(raddr(9 downto 2)));
end architecture;
