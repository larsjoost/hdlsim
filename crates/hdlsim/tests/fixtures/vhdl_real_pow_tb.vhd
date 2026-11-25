-- Real exponentiation in a constant initializer: `2.0 ** 10.0` must fold in
-- const-eval like every other real arithmetic operator (the fold previously
-- aborted with a "constant initializer is not constant" warning even though
-- the runtime evaluated it).
entity real_pow_tb is
end entity;

architecture sim of real_pow_tb is
  constant R : real    := 2.0 ** 10.0;
  constant N : natural := integer(R);
begin
  main : process
  begin
    report "N=" & integer'image(N);
    wait;
  end process;
end architecture;
