-- Regression fixture: array type indexed by an enum type mark
-- (`TYPE state_t IS ARRAY (device_under_test_t) OF std_ulogic;`) must lower
-- without "unknown identifier 'device_under_test_t'" — the index is a type,
-- not an expression. Exercises the others-aggregate constant, indexed
-- assignment, and array equality comparison too.
LIBRARY ieee;
USE ieee.std_logic_1164.ALL;

ENTITY array_enum_index IS
END ENTITY array_enum_index;

ARCHITECTURE behavior OF array_enum_index IS

  TYPE device_under_test_t IS (
    LOCALLINK_DATAWIDTH_CONVERTER,
    DELAYLINE,
    STRING_CONVERTER,
    ARBITER);

  TYPE state_t IS ARRAY (device_under_test_t) OF std_ulogic;

  CONSTANT C_ALL_ASSERTED : state_t := (OTHERS => '1');

  SIGNAL done_i : state_t;
  SIGNAL ok_i   : state_t;
  SIGNAL all_done_i : std_ulogic;

BEGIN

  done_i(LOCALLINK_DATAWIDTH_CONVERTER) <= '1';
  ok_i(DELAYLINE) <= '0';
  all_done_i <= '1' WHEN (done_i = C_ALL_ASSERTED) ELSE '0';

END ARCHITECTURE behavior;
