-- Self-checking testbench for the embedded ieee.std_logic_arith /
-- numeric_std / std.env packages: the `use` clauses must resolve and the
-- conversion/arithmetic builtins must simulate correctly.
library ieee;
use ieee.std_logic_1164.all;
use ieee.std_logic_arith.all;
use ieee.numeric_std.all;
use std.env.all;

entity tb_std_arith is
end entity;

architecture sim of tb_std_arith is
begin
    process
        variable u : unsigned(15 downto 0);
    begin
        u := conv_unsigned(200, 16);
        assert to_integer(u) = 200
            report "conv_unsigned/to_integer failed" severity error;
        u := u + 55;
        assert to_integer(u) = 255
            report "unsigned add failed" severity error;
        u := resize(u, 8);
        assert to_integer(u) = 255
            report "resize failed" severity error;
        assert (u = conv_unsigned(255, 8))
            report "unsigned equality failed" severity error;
        report "PASS: std_logic_arith";
        std.env.finish;
        wait;
    end process;
end architecture;
