library ieee ;
use ieee.std_logic_1164.all ;

package genpkg is
  generic (
    type elem_t ;
    width : integer := 8
  ) ;
  signal sig : std_logic_vector(width-1 downto 0) ;
  constant def : integer := width ;
end package ;
