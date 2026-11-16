-- Architecture referencing an entity name that does not exist: the E0401
-- caret must point at `edge_detector` itself, with a did-you-mean help for
-- the entity declared in edge_detector_entity.vhd.
ARCHITECTURE rtl OF edge_detector IS
begin
end architecture;
