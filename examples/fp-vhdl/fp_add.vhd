-- IEEE-754 single-precision (fp32) adder, multi-cycle FSM.
-- Handshake: pulse `start`; when `done` pulses, `result` holds the sum.
-- States: 0 IDLE, 1 ALIGN (shift smaller mantissa right), 2 ADD/SUB,
--         3 NORM (shift left to restore the hidden bit), 4 FINISH.
-- Denormals flush to zero; NaN -> quiet NaN; +inf/-inf passthrough (inf + -inf
-- is not detected as NaN in this minimal example).
library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;

entity fp_add is
    port (
        clk    : in  std_logic;
        start  : in  std_logic;
        a, b   : in  std_logic_vector(31 downto 0);
        result : out std_logic_vector(31 downto 0);
        done   : out std_logic
    );
end entity;

architecture rtl of fp_add is
    signal state  : integer := 0;
    signal sign_a, sign_b : std_logic := '0';
    signal sign_r : std_logic := '0';
    signal ma, mb : unsigned(23 downto 0) := (others => '0'); -- mantissas with hidden bit
    signal sum_r  : unsigned(24 downto 0) := (others => '0'); -- 25 bits: room for carry
    signal exp_r  : unsigned(8 downto 0) := (others => '0');  -- biased exponent of larger operand
    signal de     : unsigned(5 downto 0) := (others => '0');  -- alignment shift count
    signal res    : std_logic_vector(31 downto 0) := x"00000000";
    signal done_r : std_logic := '0';
begin
    result <= res;
    done   <= done_r;

    process(clk)
        variable ea, eb : unsigned(7 downto 0);
        variable d      : unsigned(7 downto 0);
        variable s      : unsigned(24 downto 0);
    begin
        if rising_edge(clk) then
            done_r <= '0';
            case state is
                when 0 => -- IDLE: latch operands, handle specials
                    if start = '1' then
                        ea := unsigned(a(30 downto 23));
                        eb := unsigned(b(30 downto 23));
                        if (ea = x"FF" and unsigned(a(22 downto 0)) /= to_unsigned(0, 23))
                           or (eb = x"FF" and unsigned(b(22 downto 0)) /= to_unsigned(0, 23)) then
                            res <= x"7FC00000"; -- NaN
                            done_r <= '1';
                        elsif ea = x"FF" then
                            res <= a;           -- inf + x = inf
                            done_r <= '1';
                        elsif eb = x"FF" then
                            res <= b;
                            done_r <= '1';
                        elsif ea = x"00" then
                            res <= b;           -- a is zero (denormals flushed)
                            done_r <= '1';
                        elsif eb = x"00" then
                            res <= a;
                            done_r <= '1';
                        else
                            -- order so that ma/exp_r belong to the larger exponent
                            if ea >= eb then
                                ma <= resize(unsigned('1' & a(22 downto 0)), 24);
                                mb <= resize(unsigned('1' & b(22 downto 0)), 24);
                                sign_a <= a(31);
                                sign_b <= b(31);
                                exp_r <= '0' & ea;
                                d := ea - eb;
                            else
                                ma <= resize(unsigned('1' & b(22 downto 0)), 24);
                                mb <= resize(unsigned('1' & a(22 downto 0)), 24);
                                sign_a <= b(31);
                                sign_b <= a(31);
                                exp_r <= '0' & eb;
                                d := eb - ea;
                            end if;
                            if d > to_unsigned(31, 8) then
                                de <= to_unsigned(31, 6); -- rest of the mantissa is lost anyway
                            else
                                de <= resize(d, 6);
                            end if;
                            state <= 1;
                        end if;
                    end if;
                when 1 => -- ALIGN: shift the smaller mantissa right, one bit per clock
                    if de = to_unsigned(0, 6) then
                        state <= 2;
                    else
                        mb <= '0' & mb(23 downto 1);
                        de <= de - to_unsigned(1, 6);
                    end if;
                when 2 => -- ADD/SUB the aligned mantissas
                    if sign_a = sign_b then
                        s := resize(ma, 25) + resize(mb, 25);
                        sign_r <= sign_a;
                        if s(24) = '1' then -- carry: shift right, bump exponent
                            s := '0' & s(24 downto 1);
                            exp_r <= exp_r + to_unsigned(1, 9);
                        end if;
                    else
                        if ma >= mb then
                            s := resize(ma, 25) - resize(mb, 25);
                            sign_r <= sign_a;
                        else
                            s := resize(mb, 25) - resize(ma, 25);
                            sign_r <= sign_b;
                        end if;
                    end if;
                    sum_r <= s;
                    state <= 3;
                when 3 => -- NORM: shift left until the hidden bit is at 23
                    if sum_r = to_unsigned(0, 25)
                       or exp_r(8) = '1' or exp_r = to_unsigned(0, 9) then
                        res <= sign_r & "00000000" & "00000000000000000000000"; -- zero/underflow
                        done_r <= '1';
                        state <= 0;
                    elsif sum_r(23) = '1' then
                        state <= 4;
                    else
                        sum_r <= sum_r(23 downto 0) & '0';
                        exp_r <= exp_r - to_unsigned(1, 9);
                    end if;
                when 4 => -- FINISH: pack sign, exponent, fraction
                    res <= sign_r & std_logic_vector(exp_r(7 downto 0))
                           & std_logic_vector(sum_r(22 downto 0));
                    done_r <= '1';
                    state <= 0;
                when others =>
                    state <= 0;
            end case;
        end if;
    end process;
end architecture;
