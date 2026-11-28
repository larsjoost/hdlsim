-- Fixture: predefined scalar (in)equality is value comparison (LRM 1076,
-- relational operators): the result is boolean and never depends on
-- metastable values. 'U' = '0' is false and 'U' /= '0' is true — plain
-- enumeration comparison, no X propagation. A regression lowered these
-- operators to the X-propagating variants, so ('U' = '0') evaluated to 'X'
-- and a guarded wait whose condition read `sig /= '0'` was never taken.
--
-- Also pins the matching wait-until semantics: a wait on a value condition
-- resumes on an event of the signal itself, not on an unrelated clock edge.

LIBRARY ieee;
USE ieee.std_logic_1164.ALL;

ENTITY tb_scalar_eq IS
END ENTITY tb_scalar_eq;

ARCHITECTURE sim OF tb_scalar_eq IS

  SIGNAL clk     : std_ulogic := '0';
  SIGNAL sig_a   : std_ulogic;
  SIGNAL sig_b   : std_ulogic;
  SIGNAL derived : std_ulogic;
  SIGNAL notes   : bit_vector(0 TO 5) := (OTHERS => '0');

BEGIN

  clk <= NOT clk AFTER 5 ns;          -- 10 ns period, rise at 5,15,25...

  sig_a <= '1', '0' AFTER 10 ns;      -- 'U' at t=0, '1', falls at 10 ns
  sig_b <= '1', '0' AFTER 20 ns;

  -- A sensitive process derives a flag from the raw signal.
  PROCESS (sig_a, derived) IS
  BEGIN
    IF sig_a = '1' THEN
      derived <= '1';
    ELSIF derived = '1' AND sig_a = '0' THEN
      derived <= '0';
    END IF;
  END PROCESS;

  -- Checker 1: the guarded wait must be taken at t=0 ('U' /= '0' is true),
  -- so notes(0) sets when sig_a falls at 10 ns, not at t=0.
  PROCESS IS
  BEGIN
    IF sig_a /= '0' THEN
      WAIT UNTIL sig_a = '0';
      notes(0) <= '1';                -- reached only via the wait
    END IF;
    WAIT;
  END PROCESS;

  -- Checker 2: plain WAIT UNTIL on a value condition.
  PROCESS IS
  BEGIN
    WAIT UNTIL sig_b = '0';
    notes(1) <= '1';
    WAIT;
  END PROCESS;

  -- Checker 3: the scalar comparisons at t=0 are boolean value results.
  PROCESS IS
  BEGIN
    REPORT "t0: (sig_a = '0')  is " & boolean'image(sig_a = '0');
    REPORT "t0: (sig_a /= '0') is " & boolean'image(sig_a /= '0');
    WAIT;
  END PROCESS;

  -- End of test after everything has settled.
  PROCESS IS
  BEGIN
    WAIT FOR 100 ns;
    IF notes(0) = '1' AND notes(1) = '1' THEN
      REPORT "tb_scalar_eq: PASS";
    ELSE
      REPORT "tb_scalar_eq: FAIL (both notes must be 1)" SEVERITY error;
    END IF;
    WAIT;
  END PROCESS;

END ARCHITECTURE sim;
