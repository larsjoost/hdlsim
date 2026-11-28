//! Tests for statements and expressions: concurrent and sequential
//! statements, generates, instantiations, waveforms, the expression grammar,
//! losslessness, and the never-panic robustness sweep.
//!
//! Shared helpers ([`parse_str`], [`no_errors`], [`ok`]) live in the sibling
//! [`super::tests`] module.

use super::tests::{no_errors, ok, parse_str};

#[test]
fn parses_named_aggregates() {
    // Named associations `(f => v, ...)`, positional, `others`, and
    // choice lists `1 | 2 => v` must all parse losslessly.
    let sexp = ok(r#"architecture rtl of m is
    type rt is record a : bit; b : bit; end record;
    signal r : rt;
begin
    r <= (a => '1', b => '0');
    r <= ('1', '0');
    r <= (others => '0');
    r <= (1 | 2 => '1', others => '0');
end architecture;
"#);
    assert!(sexp.contains("(Aggregate"), "{}", sexp);
    assert!(sexp.contains("(Others"), "{}", sexp);
}

#[test]
fn parses_all_end_architecture_forms() {
    // `end [architecture] [name];` — all four forms are valid VHDL.
    for tail in [
        "end;",
        "end rtl;",
        "end architecture;",
        "end architecture rtl;",
        "END ARCHITECTURE behavior;",
    ] {
        let src = format!("architecture rtl of m is\nbegin\n{}\n", tail);
        let sexp = ok(&src);
        assert!(sexp.contains("(Architecture"), "{}: {}", tail, sexp);
    }
}

#[test]
fn parses_generate_and_block_forms() {
    // All `end ... generate;` tails, generate declarative parts (with and
    // without `begin`), elsif/else branches in if-generate, and
    // case-generate (the non-standard `case x is generate` is tolerated).
    let src = r#"architecture rtl of m is
    signal a, b : bit_vector(3 downto 0);
begin
    g1: for i in 0 to 3 generate
        b(i) <= not a(i);
    end generate g1;
    g2: for i in 3 downto 0 generate
        signal t : bit;
    begin
        t <= a(i);
        b(i) <= t;
    end for generate;
    g3: if true generate
        b <= a;
    elsif false generate
        b <= not a;
    else generate
        b <= (others => '0');
    end if generate;
    g4: case a is generate
        when x"0" => b <= a;
        when others => b <= (others => '1');
    end case generate;
    blk: block
        signal t : bit;
    begin
        t <= '1';
    end block blk;
end architecture;
"#;
    let sexp = ok(src);
    assert_eq!(sexp.matches("(GenFor").count(), 2, "{}", sexp);
    assert_eq!(sexp.matches("(GenIf").count(), 1, "{}", sexp);
    assert_eq!(sexp.matches("(GenCase").count(), 1, "{}", sexp);
    assert_eq!(sexp.matches("(Block").count(), 1, "{}", sexp);
    assert_eq!(sexp.matches("(SignalDecl").count(), 3, "{}", sexp);
    assert_eq!(sexp.matches("(CaseItem").count(), 2, "{}", sexp);
}

#[test]
fn parses_case_and_for() {
    let src = r#"architecture rtl of m is
begin
    process
    begin
        case op is
            when x"00" => r <= a + b;
            when others => r <= a;
        end case;
        for i in 0 to 7 loop
            v(i) := '0';
        end loop;
        wait until rising_edge(clk);
    end process;
end;
"#;
    let (p, db) = parse_str(src);
    no_errors(&p, &db);
    let sexp = p.root.debug_sexp(&db);
    assert!(sexp.contains("(Case"), "{}", sexp);
    assert!(sexp.contains("Others"), "{}", sexp);
    assert!(sexp.contains("(For"), "{}", sexp);
    assert!(sexp.contains("(Wait"), "{}", sexp);
    assert!(sexp.contains("(VarAssign"), "{}", sexp);
}

#[test]
fn parses_instantiation() {
    let src = r#"architecture rtl of top is
    component counter is
        port (clk : in std_logic; q : out std_logic_vector(7 downto 0));
    end component;
begin
    u0 : counter port map (clk => clk, q => q);
end;
"#;
    let (p, db) = parse_str(src);
    no_errors(&p, &db);
    let sexp = p.root.debug_sexp(&db);
    assert!(sexp.contains("(ComponentDecl"), "{}", sexp);
    assert!(sexp.contains("(Inst"), "{}", sexp);
    assert!(sexp.contains("(PortConn"), "{}", sexp);
}

#[test]
fn expression_precedence() {
    let (p, db) = parse_str(
        "architecture rtl of m is begin p: process begin v := 1 + 2 * 3; end process; end;",
    );
    no_errors(&p, &db);
    let sexp = p.root.debug_sexp(&db);
    // `1 + 2 * 3` must nest as 1 + (2*3): the inner BinExpr contains `*`.
    let star = sexp.find("*").expect("star op present");
    let plus = sexp.find('+').expect("plus op present");
    assert!(plus < star, "{}", sexp);
}

#[test]
fn lossless_roundtrip() {
    let src =
        "entity e is port (a : in bit); end;\narchitecture r of e is begin a <= not a; end;\n";
    let (p, db) = parse_str(src);
    no_errors(&p, &db);
    assert_eq!(p.root.text(&db), src);
}
#[test]
fn parses_vhdl2008_wait_and_condition_operator() {
    // `wait on ... until ...` clause order, `??` condition operator, and a
    // parenthesized resolution function in a subtype declaration.
    let sexp = ok(r#"package p is
  subtype slv_max is (resolved_max) std_ulogic_vector;
end package p;
entity e is end entity;
architecture a of e is
  signal clk : bit := '0';
  signal en : boolean := true;
begin
  process
  begin
    wait on clk until (??(clk = '1')) and en;
    wait until ??en for 10 ns;
    wait;
  end process;
end architecture;
"#);
    assert!(sexp.contains("(Wait"), "{}", sexp);
    assert!(sexp.contains("(UnaryExpr"), "{}", sexp);
}

#[test]
fn parses_for_loop_range_expressions() {
    // Attribute arithmetic in a for-loop discrete range (`input'length-1`)
    // and plain attribute bounds (`t'left to t'right`).
    let sexp = ok(r#"entity e is end entity;
architecture a of e is
  signal input : bit_vector(7 downto 0);
begin
  process
  begin
    for i in input'length-1 downto 0 loop
      null;
    end loop;
    for j in input'left to input'right loop
      null;
    end loop;
  end process;
end architecture;
"#);
    assert!(sexp.contains("(For"), "{}", sexp);
}
#[test]
fn parses_selected_signal_assignment() {
    let sexp = ok(r#"architecture rtl of m is
begin
    with sel select
        y <= a when 0,
             b when 1 | 2,
             c when others;
end;
"#);
    assert!(sexp.contains("(SelectedAssign"), "{}", sexp);
    assert!(sexp.contains("Others"), "{}", sexp);
}

#[test]
fn parses_generate_case() {
    let sexp = ok(r#"architecture rtl of m is
begin
    case g_sel generate
        when 0 =>
            y <= a;
        when 1 | 2 =>
            y <= b;
        when others =>
            y <= c;
    end generate;
end;
"#);
    assert!(sexp.contains("(GenCase"), "{}", sexp);
    assert_eq!(sexp.matches("(CaseItem").count(), 3, "{}", sexp);
}

#[test]
fn parses_concurrent_procedure_call_postponed_and_guarded() {
    let sexp = ok(r#"architecture rtl of m is
begin
    init(x);
    postponed process(clk) begin
        q <= d;
    end process;
    blk : block (g = '1') guarded is
    begin
        q <= d;
    end block;
end;
"#);
    assert!(sexp.contains("(CallStmt"), "{}", sexp);
    assert!(sexp.contains("postponed"), "{}", sexp);
    assert!(sexp.contains("guarded"), "{}", sexp);
}

#[test]
fn parses_exit_next_labels_force_release_matching_case() {
    let sexp = ok(r#"architecture rtl of m is
begin
    p : process
    begin
        outer : loop
            exit outer when done = '1';
            next when skip;
        end loop;
        force q <= d;
        release q;
        r <= unaffected;
        case? op is
            when 0 to 3 | 7 => r <= a;
            when others => r <= b;
        end case;
    end process;
    o <= a when s = '1' else unaffected;
end;
"#);
    assert_eq!(sexp.matches("(ExitStmt").count(), 2, "{}", sexp);
    // force, release, `unaffected`, and the two case-alternative assigns.
    assert_eq!(sexp.matches("(SeqAssign").count(), 5, "{}", sexp);
    assert!(sexp.contains("(Case "), "{}", sexp);
    // Concurrent conditional waveform with a trailing `else unaffected`.
    assert!(sexp.contains("(ConcAssign"), "{}", sexp);
}

#[test]
fn parses_r19_subprog_generic_return_of_seq_block_bitstring() {
    let sexp = ok(r#"
package p is
    function f generic (type pt) parameter (s : natural; a : pt; b : a'subtype) return a'subtype;
    function g return rv_t of std_logic_vector;
    type int_vec_t is new work.pkg generic map (N => 4);
    procedure priv generic (type q) parameter (x : q);
end package;
architecture rtl of m is
    constant c : bit_vector(5 downto 0) := 6Ux"a";
begin
    p : process
    begin
        inner : block
        begin
            q <= '1';
        end block;
        case? op is
            when 0 => q <= a;
            when others => q <= b;
        end case?;
        r <= f generic map (pt => std_logic)(0, a, b);
    end process;
end;
"#);
    assert_eq!(sexp.matches("(GenericClause").count(), 2, "{}", sexp);
    assert_eq!(sexp.matches("(Block").count(), 1, "{}", sexp);
    assert!(sexp.contains("Ux"), "{}", sexp);
    assert!(sexp.contains("(Call "), "{}", sexp);
}

#[test]
fn parses_qualified_allocator_null_extname_condop() {
    let sexp = ok(r#"architecture rtl of m is
    type ptr is access integer;
    variable p : ptr;
begin
    p : process
        variable v : integer;
    begin
        v := integer'(42);
        p := new integer;
        p := new integer'(7);
        if v = 0 then v := 1; end if;
        v := (a ?? b);
        if p = null then v := 0; end if;
    end process;
    q <= << signal dut.internal_sig : std_logic >>;
end;
"#);
    assert!(sexp.contains("(Qualified"), "{}", sexp);
    assert_eq!(sexp.matches("(Allocator").count(), 2, "{}", sexp);
    assert!(sexp.contains("(CondExpr"), "{}", sexp);
    assert!(sexp.contains("(ExtName"), "{}", sexp);
}

#[test]
fn parses_instantiation_with_configuration_aspect() {
    let sexp = ok(r#"architecture rtl of top is
begin
    u0 : configuration work.cfg port map (clk => clk);
end;
"#);
    assert!(sexp.contains("(Inst"), "{}", sexp);
}

#[test]
fn never_panics_on_garbage() {
    for src in [
        "",
        "entity",
        "configuration",
        "architecture r of e is begin with select",
        "package p is new",
        "type t is record",
        "<< signal",
        "case? ",
        "v := new",
        "alias",
        "attribute",
        "for all :",
    ] {
        let _ = parse_str(src);
    }
}
#[test]
fn parses_multi_element_waveform_with_after() {
    let sexp = ok(r#"entity e is end entity;
architecture rtl of e is
  signal clk : bit := '0';
begin
  process
  begin
    clk <= '1', '0' after 5 ns, '1' after 10 ns;
    wait;
  end process;
end architecture;
"#);
    assert!(sexp.contains("(SeqAssign"), "{}", sexp);
}
#[test]
fn instantiation_label_is_inside_the_inst_node() {
    let sexp = ok(r#"entity e is end entity;
architecture rtl of e is
begin
  u0 : entity work.dut(rtl) port map (a => x);
  u1 : configuration work.cfg port map (a => x);
end architecture;
"#);
    let insts: Vec<&str> = sexp.split("(Inst").skip(1).collect();
    assert_eq!(insts.len(), 2, "{}", sexp);
    assert!(
        insts[0].contains("Ident \"u0\" Punct \":\""),
        "{}",
        insts[0]
    );
    assert!(
        insts[1].contains("Ident \"u1\" Punct \":\""),
        "{}",
        insts[1]
    );
    assert!(insts[1].contains("Ident \"configuration\""), "{}", insts[1]);
}

#[test]
fn parses_vhdl2008_conditional_expression() {
    // `when/else` in a pure expression position (here: an if-condition)
    // is a CondExpr; in a waveform it stays a flat conditional waveform.
    let sexp = ok(r#"entity e is end entity;
architecture rtl of e is
    signal v, y, a, b : bit;
begin
    y <= a when v = '1' else b;
    process(v)
    begin
        if (a when v = '1' else b) = '1' then
            y <= '1';
        end if;
    end process;
end architecture;
"#);
    let n_cond = sexp.matches("(CondExpr").count();
    assert_eq!(n_cond, 1, "{}", sexp);
}

#[test]
fn parses_case_generate_labeled_alternative_bare_block() {
    // VHDL-2008: `when lbl : choice => begin ... end lbl;`
    let sexp = ok(r#"architecture rtl of e is
begin
    g4 : case conf generate
        when g4_1 : 1 | 2 =>
            begin
                cmp : configuration work.reg_conf1
                    generic map (width => 1)
                    port map (clk => clk, q => do);
            end g4_1;
        when others =>
    end generate g4;
end architecture;
"#);
    assert!(sexp.contains("(GenCase"), "{}", sexp);
    assert_eq!(sexp.matches("(CaseItem").count(), 2, "{}", sexp);
    assert!(sexp.contains("(Block"), "{}", sexp);
    assert!(sexp.contains("Ident \"configuration\""), "{}", sexp);
}
