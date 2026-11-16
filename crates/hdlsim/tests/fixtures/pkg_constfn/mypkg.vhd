package mypkg is
  function sel_f(c : boolean; t, f : natural) return natural;
  function bits_f(n : natural) return natural;
end package;

package body mypkg is
  function sel_f(c : boolean; t, f : natural) return natural is
  begin
    if c then
      return t;
    else
      return f;
    end if;
  end function;

  function bits_f(n : natural) return natural is
  begin
    for i in 0 to 31 loop
      if (2**i) >= n then
        return i;
      end if;
    end loop;
    return 32;
  end function;
end package body;
