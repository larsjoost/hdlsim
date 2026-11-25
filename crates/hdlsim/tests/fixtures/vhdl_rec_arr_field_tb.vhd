-- Record field of a constrained array-of-record type: the field's flat
-- width must cover every element of the array type's own dims. When it
-- didn't, the field aggregated to a single element and field-of-array
-- selects folded to X past index 0 (and index 0 silently read the LAST
-- element).
package rec_arr_field_pkg is
  type field_id is (FIELD_VERSION, FIELD_TYPE, FIELD_SIZE);
  type field_t is record
    bit_size : natural;
    offset   : natural;
  end record;
  type fields_t is array (field_id) of field_t;
  type info_t is record
    field_description : fields_t;
    header_size       : natural;
  end record;
  constant INFO : info_t :=
    (field_description => ((8, 0), (16, 8), (32, 24)), header_size => 64);
  constant C_FIRST_SIZE  : natural := INFO.field_description(FIELD_VERSION).bit_size;
  constant C_SECOND_SIZE : natural := INFO.field_description(FIELD_TYPE).bit_size;
  constant C_INT_SIZE    : natural := INFO.field_description(0).bit_size;
end package;

package body rec_arr_field_pkg is
end package body;

use work.rec_arr_field_pkg.all;

entity rec_arr_field_tb is
end entity;

architecture sim of rec_arr_field_tb is
begin
  main : process
  begin
    report "FIRST=" & integer'image(C_FIRST_SIZE);
    report "SECOND=" & integer'image(C_SECOND_SIZE);
    report "INT=" & integer'image(C_INT_SIZE);
    wait;
  end process;
end architecture;
