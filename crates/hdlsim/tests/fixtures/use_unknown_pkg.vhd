-- `use` of a package that exists in no library: must produce a warning
-- (W0501), not a silent no-op — and must not fail the check.
library ieee;
use ieee.std_logic_1164.all;
use ieee.no_such_package.all;

entity use_unknown_pkg is
end entity;

architecture rtl of use_unknown_pkg is
begin
end architecture;
