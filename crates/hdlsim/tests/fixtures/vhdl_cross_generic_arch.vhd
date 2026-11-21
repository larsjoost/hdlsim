-- Architecture in a file separate from its entity: lowering happens in a
-- fresh scope, so the record type of the generic and the entity's signal
-- maps must be restored for `G_CFG.use_alt` to resolve.
library ieee;
use ieee.std_logic_1164.all;
use work.cg_pkg.all;

architecture rtl of cg_blk is
begin
  q <= not d when G_CFG.use_alt else d;
end architecture;
