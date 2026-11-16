-- Regression fixture: std_ulogic ports without any use clause. hdlsim treats
-- std_ulogic as a built-in 1-bit type (like std_logic), so this must lower
-- without E0401 too.
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
