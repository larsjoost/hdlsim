-- Testbench for the fp32 add/mul units (examples of hand-written VHDL
-- floating point). Each case pulses `start` on the DUT, waits for `done`,
-- and compares `result` against the expected IEEE-754 bit pattern.
library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;

entity tb_fp is
end entity;

architecture sim of tb_fp is
    signal clk : std_logic := '0';
    signal start_m, start_a : std_logic := '0';
    signal a, b : std_logic_vector(31 downto 0) := x"00000000";
    signal res_m, res_a : std_logic_vector(31 downto 0);
    signal done_m, done_a : std_logic;
    signal errors : integer := 0;
begin
    mul_dut : entity work.fp_mul
        port map (clk => clk, start => start_m, a => a, b => b,
                  result => res_m, done => done_m);
    add_dut : entity work.fp_add
        port map (clk => clk, start => start_a, a => a, b => b,
                  result => res_a, done => done_a);

    process
    begin
        clk <= not clk;
        wait for 5 ns;
    end process;

    process
        -- one mul case: raise start_m until done_m pulses, then compare res_m.
        -- The waits carry timeouts so a lost handshake fails loudly instead of
        -- hanging the simulation.
        -- (written out inline per case to stay within the supported subset)
    begin
        -- ===== fp_mul =====
        -- 1.5 * 2.0 = 3.0
        a <= x"3FC00000"; b <= x"40000000"; start_m <= '1';
        wait until done_m = '1' for 100 ns;
        start_m <= '0';
        if done_m /= '1' then
            report "FAIL: fp_mul handshake timeout" severity error;
            errors := errors + 1;
        end if;
        wait until done_m = '0' for 100 ns;
        if res_m = x"40400000" then
            report "PASS: fp_mul 1.5*2.0 = 3.0";
        else
            report "FAIL: fp_mul 1.5*2.0 = 0x" &
                integer'image(to_integer(unsigned(res_m))) severity error;
            errors := errors + 1;
        end if;
        -- -1.5 * 2.0 = -3.0
        a <= x"BFC00000"; b <= x"40000000"; start_m <= '1';
        wait until done_m = '1' for 100 ns;
        start_m <= '0';
        if done_m /= '1' then
            report "FAIL: fp_mul handshake timeout" severity error;
            errors := errors + 1;
        end if;
        wait until done_m = '0' for 100 ns;
        if res_m = x"C0400000" then
            report "PASS: fp_mul -1.5*2.0 = -3.0";
        else
            report "FAIL: fp_mul -1.5*2.0 = 0x" &
                integer'image(to_integer(unsigned(res_m))) severity error;
            errors := errors + 1;
        end if;
        -- 2.0 * 3.0 = 6.0
        a <= x"40000000"; b <= x"40400000"; start_m <= '1';
        wait until done_m = '1' for 100 ns;
        start_m <= '0';
        if done_m /= '1' then
            report "FAIL: fp_mul handshake timeout" severity error;
            errors := errors + 1;
        end if;
        wait until done_m = '0' for 100 ns;
        if res_m = x"40C00000" then
            report "PASS: fp_mul 2.0*3.0 = 6.0";
        else
            report "FAIL: fp_mul 2.0*3.0 = 0x" &
                integer'image(to_integer(unsigned(res_m))) severity error;
            errors := errors + 1;
        end if;
        -- 1.5 * 0.0 = +0.0
        a <= x"3FC00000"; b <= x"00000000"; start_m <= '1';
        wait until done_m = '1' for 100 ns;
        start_m <= '0';
        if done_m /= '1' then
            report "FAIL: fp_mul handshake timeout" severity error;
            errors := errors + 1;
        end if;
        wait until done_m = '0' for 100 ns;
        if res_m = x"00000000" then
            report "PASS: fp_mul 1.5*0.0 = +0.0";
        else
            report "FAIL: fp_mul 1.5*0.0 = 0x" &
                integer'image(to_integer(unsigned(res_m))) severity error;
            errors := errors + 1;
        end if;
        -- NaN * 2.0 = NaN (0x7FC00000)
        a <= x"7FC00000"; b <= x"40000000"; start_m <= '1';
        wait until done_m = '1' for 100 ns;
        start_m <= '0';
        if done_m /= '1' then
            report "FAIL: fp_mul handshake timeout" severity error;
            errors := errors + 1;
        end if;
        wait until done_m = '0' for 100 ns;
        if res_m = x"7FC00000" then
            report "PASS: fp_mul NaN*2.0 = NaN";
        else
            report "FAIL: fp_mul NaN*2.0 = 0x" &
                integer'image(to_integer(unsigned(res_m))) severity error;
            errors := errors + 1;
        end if;
        -- ===== fp_add =====
        -- 1.5 + 2.25 = 3.75
        a <= x"3FC00000"; b <= x"40100000"; start_a <= '1';
        wait until done_a = '1' for 100 ns;
        start_a <= '0';
        if done_a /= '1' then
            report "FAIL: fp_add handshake timeout" severity error;
            errors := errors + 1;
        end if;
        wait until done_a = '0' for 100 ns;
        if res_a = x"40700000" then
            report "PASS: fp_add 1.5+2.25 = 3.75";
        else
            report "FAIL: fp_add 1.5+2.25 = 0x" &
                integer'image(to_integer(unsigned(res_a))) severity error;
            errors := errors + 1;
        end if;
        -- 1.0 + (-1.0) = +0.0
        a <= x"3F800000"; b <= x"BF800000"; start_a <= '1';
        wait until done_a = '1' for 100 ns;
        start_a <= '0';
        if done_a /= '1' then
            report "FAIL: fp_add handshake timeout" severity error;
            errors := errors + 1;
        end if;
        wait until done_a = '0' for 100 ns;
        if res_a = x"00000000" then
            report "PASS: fp_add 1.0+(-1.0) = +0.0";
        else
            report "FAIL: fp_add 1.0+(-1.0) = 0x" &
                integer'image(to_integer(unsigned(res_a))) severity error;
            errors := errors + 1;
        end if;
        -- 3.75 + (-1.5) = 2.25
        a <= x"40700000"; b <= x"BFC00000"; start_a <= '1';
        wait until done_a = '1' for 100 ns;
        start_a <= '0';
        if done_a /= '1' then
            report "FAIL: fp_add handshake timeout" severity error;
            errors := errors + 1;
        end if;
        wait until done_a = '0' for 100 ns;
        if res_a = x"40100000" then
            report "PASS: fp_add 3.75+(-1.5) = 2.25";
        else
            report "FAIL: fp_add 3.75+(-1.5) = 0x" &
                integer'image(to_integer(unsigned(res_a))) severity error;
            errors := errors + 1;
        end if;
        -- 0.5 + 0.25 = 0.75
        a <= x"3F000000"; b <= x"3E800000"; start_a <= '1';
        wait until done_a = '1' for 100 ns;
        start_a <= '0';
        if done_a /= '1' then
            report "FAIL: fp_add handshake timeout" severity error;
            errors := errors + 1;
        end if;
        wait until done_a = '0' for 100 ns;
        if res_a = x"3F400000" then
            report "PASS: fp_add 0.5+0.25 = 0.75";
        else
            report "FAIL: fp_add 0.5+0.25 = 0x" &
                integer'image(to_integer(unsigned(res_a))) severity error;
            errors := errors + 1;
        end if;
        -- 1.0 + 1.0 = 2.0 (carry path)
        a <= x"3F800000"; b <= x"3F800000"; start_a <= '1';
        wait until done_a = '1' for 100 ns;
        start_a <= '0';
        if done_a /= '1' then
            report "FAIL: fp_add handshake timeout" severity error;
            errors := errors + 1;
        end if;
        wait until done_a = '0' for 100 ns;
        if res_a = x"40000000" then
            report "PASS: fp_add 1.0+1.0 = 2.0";
        else
            report "FAIL: fp_add 1.0+1.0 = 0x" &
                integer'image(to_integer(unsigned(res_a))) severity error;
            errors := errors + 1;
        end if;

        if errors = 0 then
            report "PASS: fp-vhdl add/mul all 10 cases correct";
        else
            report "FAIL: fp-vhdl -- " & integer'image(errors) & " errors" severity error;
        end if;
        std.env.finish;
        wait;
    end process;
end architecture;
