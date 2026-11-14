-- IEEE-754 single-precision (fp32) multiplier, single-cycle.
-- Handshake: pulse `start` for one clock; `done` pulses on the next clock edge
-- together with `result`. Denormal inputs/outputs flush to zero, NaN
-- propagates as quiet NaN 0x7FC00000, overflow gives inf.
library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;

entity fp_mul is
    port (
        clk    : in  std_logic;
        start  : in  std_logic;
        a, b   : in  std_logic_vector(31 downto 0);
        result : out std_logic_vector(31 downto 0);
        done   : out std_logic
    );
end entity;

architecture rtl of fp_mul is
    signal res    : std_logic_vector(31 downto 0) := x"00000000";
    signal done_r : std_logic := '0';
begin
    result <= res;
    done   <= done_r;

    process(clk)
        variable ea, eb : unsigned(7 downto 0);
        variable e      : unsigned(8 downto 0);  -- biased exponent
        variable p      : unsigned(47 downto 0); -- 24x24 product
        variable m      : unsigned(23 downto 0); -- normalized mantissa (hidden bit at 23)
        variable s      : std_logic;
    begin
        if rising_edge(clk) then
            done_r <= '0';
            if start = '1' then
                ea := unsigned(a(30 downto 23));
                eb := unsigned(b(30 downto 23));
                s  := a(31) xor b(31);
                if (ea = x"FF" and unsigned(a(22 downto 0)) /= to_unsigned(0, 23))
                   or (eb = x"FF" and unsigned(b(22 downto 0)) /= to_unsigned(0, 23)) then
                    res <= x"7FC00000"; -- NaN in -> quiet NaN out
                elsif ea = x"FF" or eb = x"FF" then
                    res <= s & "11111111" & "00000000000000000000000"; -- inf
                elsif ea = x"00" or eb = x"00" then
                    res <= s & "00000000" & "00000000000000000000000"; -- zero
                else
                    e := ('0' & ea) + ('0' & eb) - to_unsigned(127, 9);
                    -- widen operands first: a BinOp is as wide as its widest operand
                    p := resize(unsigned('1' & a(22 downto 0)), 48)
                       * resize(unsigned('1' & b(22 downto 0)), 48);
                    if p(47) = '1' then -- product >= 2: renormalize, bump exponent
                        e := e + to_unsigned(1, 9);
                        m := p(47 downto 24);
                    else
                        m := p(46 downto 23);
                    end if;
                    if e(8) = '1' or e = to_unsigned(0, 9) then
                        res <= s & "00000000" & "00000000000000000000000"; -- underflow -> 0
                    elsif e >= to_unsigned(255, 9) then
                        res <= s & "11111111" & "00000000000000000000000"; -- overflow -> inf
                    else
                        res <= s & std_logic_vector(e(7 downto 0))
                               & std_logic_vector(m(22 downto 0));
                    end if;
                end if;
                done_r <= '1';
            end if;
        end if;
    end process;
end architecture;
