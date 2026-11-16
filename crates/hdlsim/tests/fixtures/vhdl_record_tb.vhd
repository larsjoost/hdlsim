-- Self-checking testbench for record types: declaration, whole-record
-- assignment with a named aggregate, per-field read/write via `r.field`.
library ieee;
use ieee.std_logic_1164.all;
use std.env.all;

entity tb_record is
end entity;

architecture sim of tb_record is
  type bus_req_t is record
    meta : std_ulogic_vector(4 downto 0);
    stb  : std_ulogic;
    data : std_ulogic_vector(31 downto 0);
  end record;
  signal r : bus_req_t;
  signal x : std_ulogic_vector(31 downto 0);
begin
  x <= r.data;

  process
  begin
    r <= (meta => "00010", stb => '1', data => x"deadbeef");
    wait for 1 ns;
    assert r.meta = "00010" report "record field read (meta) failed" severity error;
    assert r.stb = '1' report "record field read (stb) failed" severity error;
    assert r.data = x"deadbeef" report "record field read (data) failed" severity error;
    assert x = x"deadbeef" report "record field via port failed" severity error;
    r.stb <= '0';
    r.data <= x"12345678";
    wait for 1 ns;
    assert r.stb = '0' report "record field write (stb) failed" severity error;
    assert r.data = x"12345678" report "record field write (data) failed" severity error;
    report "PASS: vhdl record fields";
    std.env.finish;
    wait;
  end process;
end architecture;
