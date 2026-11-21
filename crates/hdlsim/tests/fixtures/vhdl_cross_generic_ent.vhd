-- Entity declaring a record-typed generic with an aggregate default. The
-- architecture lives in a separate file (vhdl_cross_generic_arch.vhd).
library ieee;
use ieee.std_logic_1164.all;
use work.cg_pkg.all;

entity cg_blk is
  generic (
    G_CFG : cg_cfg_t := (use_alt => false, depth => 1));
  port (
    d : in  std_logic;
    q : out std_logic);
end entity;
