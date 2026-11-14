-- Self-checking VHDL testbench for the counter.
library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;

entity tb_counter is
end entity;

architecture sim of tb_counter is
    signal clk : std_logic := '0';
    signal rst : std_logic := '1';
    signal q   : std_logic_vector(7 downto 0);
    signal done : boolean := false;
begin
    dut : entity work.counter
        port map (clk => clk, rst => rst, q => q);

    process
    begin
        clk <= not clk;
        wait for 5 ns;
    end process;

    process
        variable errors : integer := 0;
        variable i : integer;
    begin
        wait until rising_edge(clk);
        wait until rising_edge(clk);
        rst <= '0';
        for i in 0 to 9 loop
            wait until rising_edge(clk);
            wait for 1 ns;
            if q /= std_logic_vector(to_unsigned(i + 1, 8)) then
                report "FAIL: after " & integer'image(i + 1) & " clocks q=" & integer'image(to_integer(unsigned(q))) severity error;
                errors := errors + 1;
            end if;
        end loop;
        if errors = 0 then
            report "PASS: VHDL counter counted 1..10 correctly (q=" & integer'image(to_integer(unsigned(q))) & ")";
        else
            report "FAIL: " & integer'image(errors) & " errors" severity error;
        end if;
        done <= true;
        wait;
    end process;

    process
    begin
        wait until done;
        report "VHDL sim finished";
        std.env.finish;
        wait;
    end process;
end architecture;
