-- case statement, matching case? with don't-care '-', when others.
entity tb is end entity;
architecture sim of tb is
  signal s : bit_vector(1 downto 0) := "01";
  signal r : integer := 0;
begin
  process(s)
  begin
    case s is
      when "01" => r <= 1;
      when others => r <= 99;
    end case;
  end process;
  process
  begin
    wait for 1 ns;
    if r = 1 then
      case? s is
        when "0-" => report "PASS";
        when others => report "FAIL";
      end case;
    else
      report "FAIL r";
    end if;
    wait;
  end process;
end architecture;
