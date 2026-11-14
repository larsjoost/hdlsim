-- Neural-network processing unit (NPU): an 8x8 matrix-vector accelerator
-- (one layer of a small LLM-style network: y = W * x + no bias, mod 2^32),
-- memory-mapped so the RV32I cpu drives it over its data-memory bus.
--
-- Register map (byte addresses; word index = addr(5 downto 2)):
--   0x1000 CTRL    w: bit0=1 -> start a matvec   r: reads as 0
--   0x1004 STATUS  r: bit0 = busy, bit1 = done
--   0x1010 WDATA   w: append weight  w[wptr], wptr++   (64 weights, row-major)
--   0x1014 XDATA   w: append input   x[xptr], xptr++   (8 inputs)
--   0x1018 RDATA   r: y[rptr], rptr++                   (8 results)
--   0x101C PTRS    w: 1 -> reset wptr/xptr/rptr, clear busy/done
library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;

entity npu is
    port (
        clk   : in  std_logic;
        addr  : in  unsigned(31 downto 0);   -- byte address from the cpu bus
        we    : in  std_logic;               -- bus write strobe
        re    : in  std_logic;               -- bus read strobe
        wdata : in  unsigned(31 downto 0);
        rdata : out std_logic_vector(31 downto 0)
    );
end entity;

architecture rtl of npu is
    type wmem_t is array (0 to 63) of std_logic_vector(31 downto 0);
    type vec_t  is array (0 to 7)  of std_logic_vector(31 downto 0);
    signal wmem : wmem_t := (others => x"00000000");
    signal xmem : vec_t  := (others => x"00000000");
    signal y    : vec_t  := (others => x"00000000");

    signal wptr, xptr, rptr : unsigned(7 downto 0) := x"00";
    signal busy, done : std_logic := '0';
    signal run : std_logic := '0';
    signal i, j : unsigned(7 downto 0) := x"00";
    signal acc : unsigned(31 downto 0) := x"00000000";
begin
    -- register writes, read-pointer advance, and the compute FSM (1 MAC/clk)
    process(clk)
    begin
        if rising_edge(clk) then
            if we = '1' then
                case to_integer(addr(5 downto 2)) is
                    when 0 =>  -- CTRL
                        if wdata(0) = '1' then
                            run <= '1'; busy <= '1'; done <= '0';
                            i <= x"00"; j <= x"00"; acc <= x"00000000";
                        end if;
                    when 4 =>  -- WDATA
                        wmem(to_integer(wptr)) <= std_logic_vector(wdata);
                        wptr <= wptr + 1;
                    when 5 =>  -- XDATA
                        xmem(to_integer(xptr)) <= std_logic_vector(wdata);
                        xptr <= xptr + 1;
                    when 7 =>  -- PTRS reset
                        wptr <= x"00"; xptr <= x"00"; rptr <= x"00";
                        run <= '0'; busy <= '0'; done <= '0';
                    when others => null;
                end case;
            end if;
            -- a read of RDATA advances the result pointer
            if re = '1' and to_integer(addr(5 downto 2)) = 6 then
                rptr <= rptr + 1;
            end if;
            -- compute FSM: acc += wmem[i*8+j] * xmem[j], one MAC per clock
            if run = '1' then
                if to_integer(j) < 7 then
                    acc <= acc + unsigned(wmem(to_integer(i) * 8 + to_integer(j)))
                                * unsigned(xmem(to_integer(j)));
                    j <= j + 1;
                else
                    y(to_integer(i)) <= std_logic_vector(
                        acc + unsigned(wmem(to_integer(i) * 8 + 7)) * unsigned(xmem(7)));
                    acc <= x"00000000";
                    j <= x"00";
                    i <= i + 1;
                    if to_integer(i) = 7 then
                        run <= '0'; busy <= '0'; done <= '1';
                    end if;
                end if;
            end if;
        end if;
    end process;

    -- combinational read mux
    process(addr, rptr, y, busy, done)
    begin
        case to_integer(addr(5 downto 2)) is
            when 1 =>  -- STATUS: bit0 busy, bit1 done
                rdata <= "000000000000000000000000000000" & done & busy;
            when 6 =>  -- RDATA
                rdata <= y(to_integer(rptr));
            when others =>
                rdata <= x"00000000";
        end case;
    end process;
end architecture;
