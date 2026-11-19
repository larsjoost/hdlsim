-- ieee.numeric_std_unsigned — embedded subset of the IEEE 1076-2019 package
-- (the unsigned-only view of ieee.numeric_std). The arithmetic/relational
-- operators resolve through ieee.numeric_std: use clauses are transitive
-- through packages, so `use ieee.numeric_std_unsigned.all;` also makes the
-- numeric_std items visible. `to_slv` is a kernel builtin (bit-identical
-- view as std_logic_vector).
package numeric_std_unsigned is
  use ieee.numeric_std.all;

  function to_slv (arg : unsigned) return std_logic_vector;
  function to_slv (arg : signed) return std_logic_vector;
end package numeric_std_unsigned;
