-- VHDL-2008 matching relational operators ?= ?/= ?< ?<= ?> ?>=.
entity tb is end entity;
architecture sim of tb is
  signal x : std_logic := 'X';
  signal v : std_logic_vector(3 downto 0) := "10-0";
begin
  process
  begin
    x <= '1';
    wait for 1 ns;
    if x ?= '1' and not (x ?/= '1')
       and v ?= "10-0" and v ?/= "0000"
       and "1111" ?> "1110" and "0011" ?<= "0011" and "0100" ?< "0101" and "0101" ?>= "0101"
      then report "PASS";
      else report "FAIL";
    end if;
    wait;
  end process;
end architecture;
