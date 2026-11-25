-- Record-element constraints on subprogram-local variables must not lower
-- the element name as an identifier (`t_node(name(1 to n))` — `name` is a
-- record field, not a symbol in scope). A string-typed local with a formal-
-- dependent range keeps its dyn-width folding in the same body.
library ieee;
use ieee.std_logic_1164.all;

package rec_elem_pkg is
  type node_t is record
    name  : string;
    valid : boolean;
  end record;

  constant C_NAME_LENGTH : natural := 8;

  -- Local constrained through the record-element syntax.
  function make_node(arg : string) return node_t;
  -- String local with a formal-dependent range (dyn-width path).
  function pad_to(arg : string; size : positive) return string;
end package;

package body rec_elem_pkg is
  function make_node(arg : string) return node_t is
    variable trimmed : string(1 to C_NAME_LENGTH) := (others => ' ');
    variable node    : node_t(name(1 to C_NAME_LENGTH));
    variable len     : natural;
  begin
    len := arg'length;
    if len > C_NAME_LENGTH then
      len := C_NAME_LENGTH;
    end if;
    trimmed(1 to len) := arg(1 to len);
    node.name  := trimmed;
    node.valid := true;
    return node;
  end function;

  function pad_to(arg : string; size : positive) return string is
    variable result : string(1 to size) := (others => ' ');
  begin
    if arg'length > 0 then
      result(1 to arg'length) := arg;
    end if;
    return result;
  end function;
end package body;

use work.rec_elem_pkg.all;

entity rec_elem_tb is
end entity;

architecture sim of rec_elem_tb is
  constant C_NODE : node_t := make_node("abc");
begin
  main : process
  begin
    report "NODE_VALID=" & boolean'image(C_NODE.valid);
    report "PADDED=" & pad_to("hi", 6);
    wait;
  end process;
end architecture;
