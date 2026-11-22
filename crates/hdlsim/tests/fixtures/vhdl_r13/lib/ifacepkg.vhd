library ieee ;
use ieee.std_logic_1164.all ;

package ifacepkg is
  type aw_t is record
    addr : std_logic_vector(31 downto 0) ;
    id   : std_logic_vector(7 downto 0) ;
  end record ;
  type bus_t is record
    wa : aw_t ;
    wd : std_logic_vector(31 downto 0) ;
  end record ;
  view mgrview of bus_t is
    wa : view awmgr ;
    wd : out std_logic_vector(31 downto 0) ;
  end view ;
  view awmgr of aw_t is
    addr : in std_logic_vector(31 downto 0) ;
    id   : in std_logic_vector(7 downto 0) ;
  end view ;
end package ;
