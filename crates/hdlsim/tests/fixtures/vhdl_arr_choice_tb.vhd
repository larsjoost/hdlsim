-- Constrained array-of-record constants: an enum-choice aggregate with
-- OTHERS must cover every element of the array type's dims, and record
-- fields of that array type must carry the full flat width. When they
-- didn't, the array collapsed to one element and field-of-array selects
-- folded to silently wrong values (status ok, value 0).
package arr_choice_pkg is
  type field_id is (FLD_A, FLD_B, FLD_C, FLD_D);
  type field_t is record
    active : boolean;
    size   : natural;
  end record;
  constant FIELD_DEFAULT : field_t := (active => false, size => 1);
  type fields_arr_t is array (field_id) of field_t;
  constant FIELDS_V2 : fields_arr_t := (
    FLD_B => (true, 8),
    FLD_D => (true, 4),
    OTHERS => FIELD_DEFAULT);
  type info_t is record
    version : natural;
    layout  : fields_arr_t;
  end record;
  constant INFO_V2 : info_t := (version => 2, layout => FIELDS_V2);
  constant INFO_CUR : info_t := INFO_V2;
  constant C_B_SIZE : natural := INFO_CUR.layout(FLD_B).size;
  constant C_D_SIZE : natural := INFO_CUR.layout(FLD_D).size;
  constant C_A_SIZE : natural := INFO_CUR.layout(FLD_A).size;
end package;

package body arr_choice_pkg is
end package body;

use work.arr_choice_pkg.all;

entity arr_choice_tb is
end entity;

architecture sim of arr_choice_tb is
begin
  main : process
  begin
    report "B=" & integer'image(C_B_SIZE);
    report "D=" & integer'image(C_D_SIZE);
    report "A=" & integer'image(C_A_SIZE);
    wait;
  end process;
end architecture;
