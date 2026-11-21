-- Block declarations are lexically scoped (LRM 11.2): a block-local signal
-- shadows an outer declaration of the same name instead of being dropped as a
-- duplicate, entity ports stay visible inside blocks (sensitivity lists), and
-- `process(all)` is case-insensitive. Regression: the block-local `s_r`
-- previously never registered (duplicate-name guard), so `s_r.re <= ...`
-- resolved against the outer integer signal and failed with E0401.
entity tb_block_shadow is
end entity;

architecture sim of tb_block_shadow is
  type complex_t is record
    re, im : real;
  end record;

  signal clk  : std_ulogic := '0';
  signal done : std_ulogic := '0';

  -- Same name as the block-local signal below, different type.
  signal s_r : integer := 0;
begin
  clk <= not clk after 5 ns;

  outer_b : BLOCK
    SIGNAL s_r : integer := 0;
  BEGIN
  END BLOCK;

  check_b : BLOCK IS
    SIGNAL s_r   : complex_t := (re => 1.5, im => 2.5);
    SIGNAL acc   : real := 0.0;
  BEGIN
    -- Field write on a block-local record signal (shadowed name).
    s_r.im <= 4.0;

    -- Sensitivity list inside a block must still see the entity port.
    tick : PROCESS (clk) IS
    BEGIN
      IF rising_edge(clk) THEN
        acc <= acc + s_r.re;
      END IF;
    END PROCESS tick;

    -- VHDL-2008 process(all), written in vendor upper case.
    watcher : PROCESS (ALL) IS
    BEGIN
      IF done = '1' THEN
        IF acc > 0.0 AND s_r.im = 4.0 AND s_r.re = 1.5 THEN
          REPORT "PASS: block scope shadowing";
        ELSE
          REPORT "FAIL: block scope shadowing" SEVERITY error;
        END IF;
      END IF;
    END PROCESS watcher;
  END BLOCK check_b;

  driver : PROCESS IS
  BEGIN
    WAIT FOR 30 ns;
    done <= '1';
    WAIT FOR 10 ns;
    WAIT;
  END PROCESS driver;
end architecture;
