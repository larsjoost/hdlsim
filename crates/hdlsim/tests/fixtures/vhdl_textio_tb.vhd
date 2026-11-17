-- Self-checking testbench for simulated std.textio: write/writeline to
-- `output`, and readline/read from a real file object.
-- Run from the crates/hdlsim directory (the data path is relative to it).
library ieee;
use ieee.std_logic_1164.all;
use std.textio.all;
use std.env.all;

entity tb_textio is
end entity;

architecture sim of tb_textio is
  file data : text open read_mode is "tests/fixtures/vhdl_textio_data.txt";
begin
  process
    variable l : line;
    variable n : natural;
    variable w : string(1 to 5);
    variable v : std_ulogic_vector(7 downto 0) := x"ff";
  begin
    write(l, "W=");
    write(l, 16);
    write(l, " V=");
    write(l, v);
    writeline(output, l);

    readline(data, l);
    read(l, n);
    read(l, w);
    write(l, "N=");
    write(l, n);
    write(l, " W=");
    write(l, w);
    writeline(output, l);

    assert n = 7 report "read integer failed" severity error;
    assert w = "hello" report "read string failed" severity error;
    assert endfile(data) = false report "endfile too early" severity error;

    readline(data, l);
    assert endfile(data) report "endfile not reached" severity error;

    report "PASS: vhdl textio";
    std.env.finish;
    wait;
  end process;
end architecture;
