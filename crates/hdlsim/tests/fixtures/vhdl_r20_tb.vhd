-- Self-checking regression fixture for the corpus round 20 front-end fix
-- (UVVM uvvm_util data_queue_pkg): a subtype declared in a protected-type
-- *body*'s declarative part (`subtype t_data_buffer is std_logic_vector(...)`)
-- must be registered before the state variable that uses it — the protected
-- body's first lowering pass previously skipped SubtypeDecl.
library ieee;
use ieee.std_logic_1164.all;
use std.env.all;

package r20_pkg is
  type queue_t is protected
    procedure push(v : std_logic_vector(7 downto 0));
    impure function size return natural;
  end protected;
end package;

package body r20_pkg is
  type queue_t is protected body
    subtype t_data_buffer is std_logic_vector(7 downto 0);
    variable priv_buf : t_data_buffer := (others => '0');
    variable priv_n : natural := 0;
    procedure push(v : std_logic_vector(7 downto 0)) is
    begin
      priv_buf := v;
      priv_n := priv_n + 1;
    end procedure;
    impure function size return natural is
    begin
      return priv_n;
    end function;
  end protected body;
end package body;

use work.r20_pkg.all;

entity tb_r20 is
end entity;

architecture sim of tb_r20 is
  shared variable q : queue_t;
begin
  process
  begin
    assert q.size = 0 report "initial size wrong" severity error;
    q.push("00001111");
    q.push("10101010");
    assert q.size = 2 report "size should be 2" severity error;
    report "PASS: round-20 protected-body local subtype";
    std.env.finish;
  end process;
end architecture;
