-- Regression fixture: `END ARCHITECTURE <name>;` (keyword + name) must parse,
-- not just `end architecture;` / `end <name>;`.
entity unit_tests_behavior is
end entity;

ARCHITECTURE behavior OF unit_tests_behavior IS
begin
end ARCHITECTURE behavior;
