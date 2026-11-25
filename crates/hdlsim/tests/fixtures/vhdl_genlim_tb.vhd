-- A generate range hanging on an unbound formal generic (directly, or
-- through a constant that folded to X from one) is constant per instance:
-- the generic map rebinds the generic and the range folds there. The
-- bare-context skip is therefore not warned, mirroring the
-- constant-initializer suppression.
entity genlim_blk is
  generic (G_W : positive);
end entity;

architecture rtl of genlim_blk is
  -- X-propagating arithmetic folds to X rather than failing, so this
  -- constant is bound to X in the bare context without any warning — and
  -- the generate range below hangs on it, not on the generic itself.
  constant LIM : natural := (G_W - 1) / 2;
begin
  gen : for i in 0 to LIM generate
    p : process
    begin
      report "GI=" & integer'image(i);
      wait;
    end process;
  end generate;
end architecture;

use work.all;

entity genlim_tb is
end entity;

architecture sim of genlim_tb is
begin
  u1 : entity work.genlim_blk
    generic map (G_W => 2);
end architecture;
