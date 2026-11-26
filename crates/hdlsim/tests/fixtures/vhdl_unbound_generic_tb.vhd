-- An unbound formal generic (no default) makes constants that reference it
-- unfoldable in the bare-module context, but they are constant per instance:
-- the generic map rebinds them and the fold succeeds there. The bare-context
-- failure is therefore not warned (established simulators evaluate these at
-- elaboration and stay silent), while the same initializer without the
-- generic still warns.
entity sized_blk is
  generic (G_W : positive);
end entity;

architecture rtl of sized_blk is
  -- A procedure that fails elaboration aborts the const-eval, so both
  -- constants below stay X in the bare context — the difference is only
  -- the warning.
  procedure setr(variable r : out natural) is
  begin
    report "setr: not constant" severity failure;
    r := 1;
  end procedure;

  function dep(v : natural) return natural is
    variable r : natural := 0;
  begin
    setr(r);
    return r + v;
  end function;

  constant C_W   : natural := dep(G_W);  -- unbound generic: not warned
  constant C_PLN : natural := dep(8);    -- plain arg: still warned
begin
  main : process
  begin
    report "CW=" & integer'image(C_W);
    wait;
  end process;
end architecture;
