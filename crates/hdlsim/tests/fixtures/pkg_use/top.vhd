-- Entity that only compiles if `use work.mypkg.all;` makes the package's
-- type visible across files (mypkg.vhd is resolved on demand from -L work=).
library work;
use work.mypkg.all;

entity top is
    port (
        clk : in  std_logic;
        st  : out state_t
    );
end entity;

architecture rtl of top is
    signal s : state_t := idle;
begin
    st <= s;
end architecture;
