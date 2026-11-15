-- Sequential statements: if/elsif/else, for loop, while loop, loop/exit, function.
entity tb is end entity;
architecture sim of tb is
  function max(a, b : integer) return integer is
  begin
    if a > b then return a; else return b; end if;
  end function;
  signal done : bit := '0';
begin
  process
    variable s : integer := 0;
    variable i : integer := 0;
  begin
    if 3 > 2 then s := s + 1; elsif 2 > 3 then s := s + 100; else s := s + 50; end if;
    for k in 0 to 4 loop
      s := s + k;
    end loop;
    i := 0;
    while i < 3 loop
      i := i + 1;
    end loop;
    loop
      s := s + 1;
      exit when s > 20;
    end loop;
    if s = 21 and i = 3 and max(7, 9) = 9 then
      report "PASS";
    else
      report "FAIL";
    end if;
    done <= '1';
    wait;
  end process;
end architecture;
