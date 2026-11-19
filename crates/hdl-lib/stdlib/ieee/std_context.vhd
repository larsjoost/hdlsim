-- ieee.std_context — embedded subset of the IEEE 1076-2008 std_context
-- (a context declaration, not a package), so `context ieee.ieee_std_context;`
-- resolves. std.standard is always visible (the implicit prelude).
context ieee_std_context is
  library ieee;
  use ieee.std_logic_1164.all;
  use ieee.numeric_std.all;
end context ieee_std_context;
