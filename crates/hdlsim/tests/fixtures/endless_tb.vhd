-- Regression fixture: a design that runs forever by design (a stimulus
-- process that keeps scheduling events). Used to prove that a bounded run
-- (--max-wall) or an external interrupt (SIGINT/SIGTERM) stops the kernel
-- and reports where the simulation was — process origin and time — instead
-- of dying silently or exiting clean.
library ieee;
use ieee.std_logic_1164.all;

entity endless_tb is
end entity;

architecture sim of endless_tb is
  signal tick : std_logic := '0';
begin

  -- Time advances forever: there is always a next event, so a plain run
  -- never terminates on its own.
  stim : PROCESS
  BEGIN
    while true loop
      tick <= not tick;
      wait for 1 us;
    end loop;
  END PROCESS;

end architecture;
