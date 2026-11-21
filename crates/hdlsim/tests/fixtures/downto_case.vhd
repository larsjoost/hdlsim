-- Regression fixture: descending ranges must size by the keyword, and the
-- keyword match must be case-insensitive (VHDL keywords are case-insensitive).
LIBRARY ieee;
USE ieee.std_logic_1164.all;

ENTITY tb_downto_case IS
END ENTITY tb_downto_case;

ARCHITECTURE sim OF tb_downto_case IS
  SIGNAL v_upper : std_ulogic_vector(3 DOWNTO 0);
  SIGNAL v_lower : std_ulogic_vector(3 downto 0);
  TYPE t_upper IS ARRAY (3 DOWNTO 0) OF std_ulogic;
  TYPE t_lower IS ARRAY (3 downto 0) OF std_ulogic;
  SIGNAL a_upper : t_upper;
  SIGNAL a_lower : t_lower;
BEGIN
  v_upper(3) <= '1';
  v_lower(0) <= '1';
  a_upper(0) <= '1';
  a_lower(3) <= '1';
END ARCHITECTURE sim;
