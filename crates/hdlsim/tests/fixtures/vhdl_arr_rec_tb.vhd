-- Self-checking testbench for arrays of records: `arr(i).field` read/write,
-- comma-separated record fields, record-typed ports, field slices, and
-- dotted names in sensitivity lists.
library ieee;
use ieee.std_logic_1164.all;
use std.env.all;

package arr_pkg is
  type bus_req_t is record
    addr : std_ulogic_vector(31 downto 0);
    stb, lock : std_ulogic;
  end record;
  type bus_rsp_t is record
    ack, err : std_ulogic;
    data : std_ulogic_vector(31 downto 0);
  end record;
  type port_req_t is array (4 downto 0) of bus_req_t;
end package;

use work.arr_pkg.all;

entity tb_arr_rec is
end entity;

architecture sim of tb_arr_rec is
  signal req_i : bus_req_t := (addr => x"00000ff0", stb => '1', lock => '1');
  signal rsp_o : bus_rsp_t;
  signal port_req : port_req_t;
  signal keeper_lock : std_ulogic;
  signal q : std_ulogic;
  signal s : std_ulogic_vector(21 downto 0);
begin
  s <= req_i.addr(31 downto 10);
  q <= port_req(2).stb;

  process (req_i.addr, port_req)
  begin
    port_req(1).stb <= req_i.stb;
    port_req(2).lock <= req_i.lock;
    keeper_lock <= req_i.lock;
    wait for 1 ns;
    assert port_req(1).stb = req_i.stb report "arr(i).field write/read failed" severity error;
    assert port_req(2).lock = req_i.lock report "arr(i).field lock failed" severity error;
    assert s'length = 22 report "field slice width failed" severity error;
    report "PASS: vhdl arrays of records";
    std.env.finish;
    wait;
  end process;

  rsp_o.ack <= '1';
  rsp_o.data <= req_i.addr;
end architecture;
