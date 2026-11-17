-- Constant arrays (of records) indexed at runtime, and array signal
-- initializers applied at simulation start.
entity tb_const_recarr is end entity;

architecture sim of tb_const_recarr is
  type rec_t is record
    opcode : bit_vector(7 downto 0);
    name   : string(1 to 3);
  end record;
  type arr_t is array (1 downto 0) of rec_t;
  constant inst_c : arr_t := (0 => (x"13", "add"), 1 => (x"33", "sub"));
  type word_arr_t is array (0 to 1) of bit_vector(7 downto 0);
  constant rom_c : word_arr_t := (x"aa", x"bb");
  signal arr : word_arr_t := (x"11", x"22");  -- array signal initializer
  signal w : bit_vector(7 downto 0);
  signal m : string(1 to 3);

  function pick(i : integer) return bit_vector is
    constant lut : word_arr_t := (x"01", x"02");  -- function-local const array
  begin
    return lut(i);
  end function;
begin
  process
  begin
    wait for 10 ns;
    w <= inst_c(1).opcode;   -- const array of records, runtime index + field
    m <= inst_c(0).name;
    wait for 10 ns;
    report "w=" & to_hstring(w) & " m=" & m;
    w <= arr(1);             -- array signal initializer
    wait for 10 ns;
    w <= rom_c(0) or pick(1);
    wait for 10 ns;
    if inst_c(1).opcode = x"33" and m = "add" and arr(1) = x"22" and pick(1) = x"02"
       and rom_c(1) = x"bb" and w = x"aa" then
      report "PASS: vhdl const record arrays";
    else
      report "FAIL: w=" & to_hstring(w) & " m=" & m;
    end if;
    wait;
  end process;
end architecture;
