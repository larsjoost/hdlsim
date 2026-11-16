-- Entity for the unknown-architecture regression test, declared in its own
-- file (like the reported edge_detector_entity.vhd).
entity edge_detector_entity is
    port (
        clk  : IN  std_ulogic;
        rst  : IN  std_ulogic;
        din  : IN  std_ulogic;
        dout : OUT std_ulogic
    );
end entity;
