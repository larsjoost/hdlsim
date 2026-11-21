-- Entity for the sibling-architecture variant test: an instantiation's
-- generic map must re-lower this entity AND its architecture, which lives
-- in a separate file (vhdl_variant_arch_rtl.vhd).
library ieee;
use ieee.std_logic_1164.all;

entity va_blk is
  generic (
    G_INV : boolean := false);
  port (
    d : in  std_logic;
    q : out std_logic);
end entity;
