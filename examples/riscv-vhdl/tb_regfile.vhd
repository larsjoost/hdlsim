-- Unit testbench for regfile.vhd: write/read several registers, x0 stays 0.
library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;

entity tb_regfile is
end entity;

architecture sim of tb_regfile is
    signal clk : std_logic := '0';
    signal we : std_logic := '0';
    signal ra1, ra2, wa : unsigned(4 downto 0) := "00000";
    signal wd : unsigned(31 downto 0) := x"00000000";
    signal rd1, rd2 : unsigned(31 downto 0);
    signal errors : integer := 0;
begin
    dut : entity work.regfile
        port map (clk => clk, we => we, ra1 => ra1, ra2 => ra2, wa => wa,
                  wd => wd, rd1 => rd1, rd2 => rd2);

    process
    begin
        clk <= not clk;
        wait for 5 ns;
    end process;

    process
    begin
        -- write x1 = 42, x2 = 7, x5 = 0xDEADBEEF
        we <= '1';
        wait until rising_edge(clk);
        wa <= "00001"; wd <= x"0000002A"; wait until rising_edge(clk);
        wa <= "00010"; wd <= x"00000007"; wait until rising_edge(clk);
        wa <= "00101"; wd <= x"DEADBEEF"; wait until rising_edge(clk);
        -- attempt to write x0 (must be ignored)
        wa <= "00000"; wd <= x"FFFFFFFF"; wait until rising_edge(clk);
        we <= '0';
        wait until rising_edge(clk);
        -- read back
        ra1 <= "00001"; ra2 <= "00010"; wait for 1 ns;
        if rd1 /= x"0000002A" then report "FAIL regfile x1" severity error; errors := errors + 1; end if;
        if rd2 /= x"00000007" then report "FAIL regfile x2" severity error; errors := errors + 1; end if;
        ra1 <= "00101"; ra2 <= "00000"; wait for 1 ns;
        if rd1 /= x"DEADBEEF" then report "FAIL regfile x5" severity error; errors := errors + 1; end if;
        if rd2 /= x"00000000" then report "FAIL regfile x0 not zero" severity error; errors := errors + 1; end if;
        -- read uninitialized register x9 -> 0 (mem initialized to 0)
        ra1 <= "01001"; wait for 1 ns;
        if rd1 /= x"00000000" then report "FAIL regfile x9 init" severity error; errors := errors + 1; end if;
        -- overwrite x1 = 100 and re-read
        we <= '1';
        wait until rising_edge(clk);
        wa <= "00001"; wd <= x"00000064"; wait until rising_edge(clk);
        we <= '0'; wait until rising_edge(clk);
        ra1 <= "00001"; ra2 <= "00001"; wait for 1 ns;
        if rd1 /= x"00000064" then report "FAIL regfile overwrite" severity error; errors := errors + 1; end if;
        if rd2 /= x"00000064" then report "FAIL regfile dual read" severity error; errors := errors + 1; end if;

        if errors = 0 then
            report "PASS: regfile — 7 checks ok";
        else
            report "FAIL: regfile — " & integer'image(errors) & " errors" severity error;
        end if;
        std.env.finish;
        wait;
    end process;
end architecture;
