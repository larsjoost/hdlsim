-- 8-bit up-counter with synchronous reset, in VHDL.
library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;

entity counter is
    port (
        clk : in  std_logic;
        rst : in  std_logic;
        q   : out std_logic_vector(7 downto 0)
    );
end entity;

architecture rtl of counter is
    signal q_s : unsigned(7 downto 0) := x"00";
begin
    process(clk)
    begin
        if rising_edge(clk) then
            if rst = '1' then
                q_s <= x"00";
            else
                q_s <= q_s + 1;
            end if;
        end if;
    end process;
    q <= std_logic_vector(q_s);
end architecture;
