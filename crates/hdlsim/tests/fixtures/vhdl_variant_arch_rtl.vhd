-- Architecture of va_blk in a file separate from its entity. The elab-time
-- variant re-lowering must include sibling architecture files, or the
-- generic-mapped instance has no body.
library ieee;
use ieee.std_logic_1164.all;

architecture rtl of va_blk is
begin
  q <= not d when G_INV else d;
end architecture;
