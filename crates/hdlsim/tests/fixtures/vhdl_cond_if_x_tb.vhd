-- An `if` over an unknown condition takes the else path, both at runtime
-- (VHDL LRM 1076 §10.8: only TRUE runs the sequence; Verilog: x/z selects
-- else) and in const-eval. A function whose only unknown is an unbound
-- generic argument therefore folds in the bare-module context instead of
-- aborting with a misleading "constant initializer is not constant" warning.
entity cond_blk is
  generic (G_W : positive);
end entity;

architecture rtl of cond_blk is
  function sel(v : integer) return natural is
  begin
    if v > 0 then
      return 1;
    else
      return 7;
    end if;
  end function;

  -- Bare context: v = X, the condition is not TRUE, so the fold takes the
  -- else path and C_X folds to 7 with no warning.
  constant C_X : natural := sel(G_W);
begin
  main : process
  begin
    report "CX=" & integer'image(C_X);
    wait;
  end process;
end architecture;

use work.all;

entity cond_if_x_tb is
end entity;

architecture sim of cond_if_x_tb is
begin
  u1 : entity work.cond_blk
    generic map (G_W => 16);
end architecture;
