-- Minimized copy of the patterns from Vivado's std_logic_arith.vhd that
-- issue #106 reported: functions with parameter-dependent local declarations
-- (`variable L : UNSIGNED(A'LENGTH-1 downto 0);`), function-local constants
-- with parameter-dependent initializers (`constant msb : integer :=
-- min(arg'length, size) - 1;`, `constant rMsb : integer := size - 1;`), a
-- qualified expression inside a concatenation (`'0' & SIGNED'(ABS(B))`), and
-- real literals with digit separators (`2.71828_18284_...`). `hdlsim check`
-- must accept this file with zero errors and zero warnings.
library ieee;
use ieee.std_logic_1164.all;
use ieee.std_logic_arith.all;

package vivado_like_arith is
  function make_binary(a : unsigned) return unsigned;
  function pad_signed(b : signed) return std_logic_vector;
  function conv_u(arg : unsigned; size : integer) return unsigned;
  constant math_e : real := 2.71828_18284_59045_23536_02874_71352;
end package;

package body vivado_like_arith is
  function make_binary(a : unsigned) return unsigned is
    variable l : unsigned(a'length - 1 downto 0);
  begin
    l := a;
    return l;
  end function;

  function pad_signed(b : signed) return std_logic_vector is
  begin
    return '0' & signed'(abs(b));
  end function;

  function conv_u(arg : unsigned; size : integer) return unsigned is
    constant msb  : integer := min(arg'length, size) - 1;
    constant rmsb : integer := size - 1;
    variable r : unsigned(msb downto 0);
  begin
    r := resize(arg, size);
    if rmsb >= 0 then
      return r;
    end if;
    return r;
  end function;
end package body;

entity vivado_like_user is
  port (
    x : in  unsigned(7 downto 0);
    y : out std_logic_vector(7 downto 0)
  );
end entity;

architecture rtl of vivado_like_user is
  signal u  : unsigned(7 downto 0);
  signal u8 : unsigned(7 downto 0);
begin
  u <= make_binary(x);
  u8 <= conv_u(x, 8);
  y <= pad_signed(signed(u));
end architecture;
