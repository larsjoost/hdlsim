-- Data memory: 256 words, asynchronous read (cpu port + testbench port),
-- synchronous write.
library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;

entity dmem is
    port (
        clk   : in  std_logic;
        we    : in  std_logic;
        waddr : in  unsigned(31 downto 0);          -- byte address from cpu
        wdata : in  unsigned(31 downto 0);
        raddr : in  unsigned(31 downto 0);          -- cpu read port
        rdata : out std_logic_vector(31 downto 0);
        taddr : in  unsigned(7 downto 0);           -- testbench read port (word index)
        tdata : out std_logic_vector(31 downto 0)
    );
end entity;

architecture rtl of dmem is
    type mem_t is array (0 to 255) of std_logic_vector(31 downto 0);
    signal mem : mem_t := (others => x"00000000");
begin
    process(clk)
    begin
        if rising_edge(clk) then
            if we = '1' then
                mem(to_integer(waddr(9 downto 2))) <= std_logic_vector(wdata);
            end if;
        end if;
    end process;

    rdata <= mem(to_integer(raddr(9 downto 2)));
    tdata <= mem(to_integer(taddr));
end architecture;
