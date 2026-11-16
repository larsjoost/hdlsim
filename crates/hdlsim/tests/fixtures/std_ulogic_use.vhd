-- Regression fixture: std_ulogic ports with the ieee.std_logic_1164 use clause
-- (the common real-world form). Must lower without E0401.
library ieee;
use ieee.std_logic_1164.all;

entity edge_detector_entity is
    port (
        clk  : IN  std_ulogic;
        rst  : IN  std_ulogic;
        din  : IN  std_ulogic;
        dout : OUT std_ulogic
    );
end entity;

architecture rtl of edge_detector_entity is
begin
    dout <= din;
end architecture;
