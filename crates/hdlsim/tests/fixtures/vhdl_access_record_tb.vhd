-- Self-checking testbench for access types / nested records / implicit globals:
--  * `p.next` on an access-to-record (implicit `.all` dereference) — compile-level
--    (access values are not simulated, so `walk` is only compiled, not called
--    with a real pointer)
--  * nested record field select `rv.param.val` on a record-typed signal
--  * bare physical unit `ns` as a literal
--  * bare `GSR` (Xilinx implicit global)
library ieee;
use ieee.std_logic_1164.all;
use std.env.all;

package acc_pkg is
  type inner_t is record
    val : integer;
  end record;
  type outer_t is record
    param : inner_t;
    flag  : std_ulogic;
  end record;
  type node_t;
  type node_ptr is access node_t;
  type node_t is record
    data : integer;
    next : node_ptr;
  end record;
  procedure walk(head : node_ptr);
end package;

package body acc_pkg is
  procedure walk(head : node_ptr) is
    variable p : node_ptr := head;
  begin
    while p /= null loop
      p := p.next; -- implicit dereference: p.all.next
    end loop;
  end procedure;
end package body;

entity tb_access is
end entity;

architecture sim of tb_access is
  use work.acc_pkg.all;
  signal rv : outer_t;
  signal gsr_seen : std_ulogic;
begin
  gsr_seen <= GSR; -- Xilinx implicit global (auto-instantiated glbl)

  process
    variable v : outer_t;
    variable u : time := ns; -- bare physical unit name = 1 unit
  begin
    v := (param => (val => 42), flag => '1');
    rv <= v;
    wait for 1 ns;
    if rv.param.val /= 42 then
      report "FAIL: nested record field read" severity failure;
    end if;
    if u /= 1 ns then
      report "FAIL: bare ns unit literal" severity failure;
    end if;
    walk(null);
    report "PASS: vhdl access/record/gsr";
    std.env.stop;
  end process;
end architecture;
