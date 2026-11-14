-- Unit test for the NPU: load wmem[k] = k, xmem[j] = 1, run one matvec,
-- expect y[i] = sum_j w[i*8+j]*x[j] = 64*i + 28.
library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;

entity tb_npu is
end entity;

architecture sim of tb_npu is
    signal clk : std_logic := '0';
    signal addr : unsigned(31 downto 0) := x"00000000";
    signal we, re : std_logic := '0';
    signal wdata : unsigned(31 downto 0) := x"00000000";
    signal rdata : std_logic_vector(31 downto 0);
    signal errors : integer := 0;
    signal done_check : std_logic := '0';
begin
    dut : entity work.npu
        port map (clk => clk, addr => addr, we => we, re => re,
                  wdata => wdata, rdata => rdata);

    process
    begin
        clk <= not clk;
        wait for 5 ns;
    end process;

    process
        variable k : integer;
    begin
        -- reset pointers
        addr <= x"0000101C"; wdata <= x"00000001"; we <= '1';
        wait until rising_edge(clk);
        we <= '0';
        wait until rising_edge(clk);
        -- load 64 weights w[k] = k via WDATA (0x1010)
        addr <= x"00001010";
        for k in 0 to 63 loop
            wdata <= to_unsigned(k, 32);
            we <= '1';
            wait until rising_edge(clk);
        end loop;
        we <= '0';
        -- load 8 inputs x[j] = 1 via XDATA (0x1014)
        addr <= x"00001014";
        for k in 0 to 7 loop
            wdata <= x"00000001";
            we <= '1';
            wait until rising_edge(clk);
        end loop;
        we <= '0';
        wait until rising_edge(clk);
        -- start
        addr <= x"00001000"; wdata <= x"00000001"; we <= '1';
        wait until rising_edge(clk);
        we <= '0';
        -- poll STATUS (0x1004) until done (bit1)
        addr <= x"00001004"; re <= '1';
        while rdata(1) /= '1' loop
            wait until rising_edge(clk);
        end loop;
        re <= '0';
        -- read 8 results via RDATA (0x1018); rptr auto-increments per read edge
        addr <= x"00001018"; re <= '1';
        for k in 0 to 7 loop
            wait for 1 ns;  -- comb read settles: rdata = y[k]
            if rdata = std_logic_vector(to_unsigned(64 * k + 28, 32)) then
                report "PASS: npu y[" & integer'image(k) & "]=" & integer'image(64 * k + 28);
            else
                report "FAIL: npu y[" & integer'image(k) & "]=" &
                    integer'image(to_integer(unsigned(rdata))) &
                    " expected " & integer'image(64 * k + 28) severity error;
                errors := errors + 1;
            end if;
            wait until rising_edge(clk);  -- rptr advances to k+1
        end loop;
        if errors = 0 then
            report "PASS: npu matvec 8x8 (w[k]=k, x=1) all 8 results correct";
        else
            report "FAIL: npu — " & integer'image(errors) & " errors" severity error;
        end if;
        std.env.finish;
        wait;
    end process;
end architecture;
