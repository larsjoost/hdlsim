-- Package holding the record type used as a generic: the type name must
-- survive into the architecture's scope when the architecture is lowered
-- from a different file, so `G_CFG.field` selects resolve.
library ieee;
use ieee.std_logic_1164.all;

package cg_pkg is
  type cg_cfg_t is record
    use_alt : boolean;
    depth   : integer;
  end record;
end package;
