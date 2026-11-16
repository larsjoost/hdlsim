-- User package for the cross-file `use work.mypkg.all;` regression test.
package mypkg is
    type state_t is (idle, run, done);
    constant WIDTH : integer := 8;
end package;
