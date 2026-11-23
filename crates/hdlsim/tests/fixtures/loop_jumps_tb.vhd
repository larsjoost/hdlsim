-- Fixture: runtime loop-jump semantics. `next` skips to the loop step (LRM
-- 1076 §10.12), `exit` leaves the loop but not the process (§10.11). A
-- regression lowered `next` to `exit`, ending the loop on the first skip.
entity loop_jumps_tb is
end entity;

architecture sim of loop_jumps_tb is
begin
  main : process
    variable odd_v : natural := 0;
    variable even_v : natural := 0;
    variable stop_v : natural := 0;
    variable tail_v : natural := 0;
  begin
    -- Unconditional next: even iterations are skipped, the loop advances.
    for i in 0 to 9 loop
      if (i mod 2) = 0 then
        next;
      end if;
      odd_v := odd_v + 1;
    end loop;
    -- Conditional next (`next when c;`): odd iterations skipped.
    for i in 0 to 9 loop
      next when (i mod 2) = 1;
      even_v := even_v + 1;
    end loop;
    -- Exit: the loop ends early, the process continues after it.
    for i in 0 to 9 loop
      if (i = 4) then
        exit;
      end if;
      stop_v := stop_v + 1;
    end loop;
    tail_v := 1;
    report "JUMPS odd=" & integer'image(odd_v)
      & " even=" & integer'image(even_v)
      & " stop=" & integer'image(stop_v)
      & " tail=" & integer'image(tail_v);
    wait;
  end process;
end architecture;
