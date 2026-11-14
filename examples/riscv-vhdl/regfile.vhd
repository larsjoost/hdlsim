-- RV32I register file: 32 x 32-bit, x0 hardwired to zero.
-- Asynchronous reads, synchronous write on rising clk edge.
library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;

entity regfile is
    port (
        clk  : in  std_logic;
        we   : in  std_logic;                      -- write enable
        ra1  : in  unsigned(4 downto 0);           -- read addr 1
        ra2  : in  unsigned(4 downto 0);           -- read addr 2
        wa   : in  unsigned(4 downto 0);           -- write addr
        wd   : in  unsigned(31 downto 0);          -- write data
        rd1  : out unsigned(31 downto 0);
        rd2  : out unsigned(31 downto 0)
    );
end entity;

architecture rtl of regfile is
    type reg_t is array (0 to 31) of std_logic_vector(31 downto 0);
    signal regs : reg_t := (others => x"00000000");
begin
    process(clk)
    begin
        if rising_edge(clk) then
            if we = '1' and to_integer(wa) /= 0 then
                regs(to_integer(wa)) <= std_logic_vector(wd);
            end if;
        end if;
    end process;

    process(ra1, ra2)
    begin
        if to_integer(ra1) = 0 then
            rd1 <= x"00000000";
        else
            rd1 <= unsigned(regs(to_integer(ra1)));
        end if;
        if to_integer(ra2) = 0 then
            rd2 <= x"00000000";
        else
            rd2 <= unsigned(regs(to_integer(ra2)));
        end if;
    end process;
end architecture;
