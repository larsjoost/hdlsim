-- VHDL-2008/2019 constructs used by OSVVM: std.standard additions (vector
-- types, file_open_kind/status, char literals like LF, `now`), generic
-- package with type parameters, incomplete + protected types, subprogram
-- aliases with signatures, function-local constants referencing parameters,
-- record fields on parameters.
package osvvmish_pkg is
  generic (
    type expected_t ;
    constant width_c : integer := 8
  );
  type mode_t is (DISABLED, ENABLED);
  type rec_t is record
    id : integer;
  end record;
  type incomplete_t;
  type prot_t is protected
    procedure poke;
  end protected;
  function double(x : integer) return integer;
  alias dbl is double [integer return integer];
end package;

package body osvvmish_pkg is
  type incomplete_t is record id : integer; end record;
  type prot_t is protected body
    procedure poke is begin null; end procedure;
  end protected body;
  function double(x : integer) return integer is
    constant two_x : integer := 2 * x;  -- function-local const referencing a param (lowers; not const-evaluable)
    variable r : incomplete_t;
  begin
    r.id := x;
    return 2 * r.id;
  end function;
end package;

package osvvmish_inst is new work.osvvmish_pkg generic map (expected_t => integer);

entity tb_osvvmish is end entity;

architecture sim of tb_osvvmish is
  use work.osvvmish_inst.all;
  signal v : integer_vector(3 downto 0) := (others => 5);
  signal c : character := LF;
  signal st : file_open_status := OPEN_OK;
  signal kind : file_open_kind := READ_MODE;
  signal mode : mode_t := ENABLED;
  signal r : rec_t;
  signal t1 : time;

  -- function-local constant referencing a parameter + record local
  -- (lowered; runtime use of record locals is a separate gap)
  function double(x : integer) return integer is
    constant two_x : integer := 2 * x;
    variable rr : rec_t;
  begin
    rr.id := x;
    return 2 * rr.id;
  end function;
begin
  r.id <= 42;

  process
  begin
    wait for 1 ns;
    t1 <= now;
    report "r.id=" & integer'image(r.id) severity note;
    assert r.id = 42 report "record field on param failed" severity failure;
    report "PASS: vhdl osvvmish constructs" severity note;
    wait;
  end process;
end architecture;
