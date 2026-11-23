-- Fixture: named use clauses merge only these two functions into scope;
-- their bodies' internal calls must still const-evaluate (package-wide
-- fallback), and the exit loop must keep its accumulated value.
library work;
use work.ce_fallback_pkg.get_chain_delay;
use work.ce_fallback_pkg.get_first_stop_delay;
use work.ce_fallback_pkg.get_odd_count_delay;
use work.ce_fallback_pkg.get_even_count_delay;

entity ce_fallback_tb is
end entity;

architecture sim of ce_fallback_tb is
  constant C_CHAIN : natural := get_chain_delay;
  constant C_STOP  : natural := get_first_stop_delay(10);
  constant C_ODD   : natural := get_odd_count_delay(10);
  constant C_EVEN  : natural := get_even_count_delay(10);
begin
  main : process
  begin
    report "C_CHAIN=" & integer'image(C_CHAIN);
    report "C_STOP=" & integer'image(C_STOP);
    report "C_ODD=" & integer'image(C_ODD);
    report "C_EVEN=" & integer'image(C_EVEN);
    wait;
  end process;
end architecture;
