//! Tests for design units and declarations: entities, architectures,
//! packages, configurations, context clauses, and every declaration form
//! (types, subtypes, files, aliases, attributes, views, interfaces).
//!
//! Statement/expression/robustness coverage lives in the sibling
//! [`super::tests_stmt`] module; [`parse_str`], [`no_errors`], and [`ok`] are
//! the shared helpers.
//!
//! Assertions check the lossless CST via `debug_sexp` snapshots.

use super::*;
use crate::source::SourceDb;

pub(super) fn parse_str(src: &str) -> (ParsedFile, SourceDb) {
    let mut db = SourceDb::new();
    let id = db.add_file("t.vhd", src).unwrap();
    (parse_file(id, db.file(id)), db)
}
pub(super) fn no_errors(p: &ParsedFile, db: &SourceDb) {
    assert!(
        !p.diags.iter().any(|d| d.severity == Severity::Error),
        "{:?}",
        p.diags.iter().map(|d| d.render(db)).collect::<Vec<_>>()
    );
}

/// Parse, assert zero errors + losslessness, return the s-expression.
pub(super) fn ok(src: &str) -> String {
    let (p, db) = parse_str(src);
    no_errors(&p, &db);
    assert_eq!(p.root.text(&db), src, "tree must be lossless");
    p.root.debug_sexp(&db)
}

#[test]
fn parses_entity_with_ports() {
    let src = r#"library ieee;
use ieee.std_logic_1164.all;
entity counter is
    port (
        clk : in  std_logic;
        rst : in  std_logic;
        q   : out std_logic_vector(7 downto 0)
    );
end entity;
"#;
    let (p, db) = parse_str(src);
    no_errors(&p, &db);
    let sexp = p.root.debug_sexp(&db);
    assert!(sexp.contains("(Entity"), "{}", sexp);
    assert!(sexp.contains("(PortClause"), "{}", sexp);
    assert!(sexp.contains("(PortDecl"), "{}", sexp);
    assert!(sexp.contains("(Range"), "{}", sexp);
}

#[test]
fn parses_architecture_with_process() {
    let src = r#"architecture rtl of counter is
    signal q_s : unsigned(7 downto 0) := x"00";
begin
    process(clk, rst)
    begin
        if rst = '1' then
            q_s <= x"00";
        elsif rising_edge(clk) then
            q_s <= q_s + 1;
        end if;
    end process;
    q <= std_logic_vector(q_s);
end architecture;
"#;
    let (p, db) = parse_str(src);
    no_errors(&p, &db);
    let sexp = p.root.debug_sexp(&db);
    assert!(sexp.contains("(Architecture"), "{}", sexp);
    assert!(sexp.contains("(Process"), "{}", sexp);
    assert!(sexp.contains("(SensList"), "{}", sexp);
    assert!(sexp.contains("(SeqAssign"), "{}", sexp);
    assert!(sexp.contains("(ConcAssign"), "{}", sexp);
    assert!(sexp.contains("(If"), "{}", sexp);
    assert!(sexp.contains("(Call"), "{}", sexp); // rising_edge(...)
}

#[test]
fn parses_vhdl2008_parameter_keyword_and_sized_bitstrings() {
    let src = r#"package p is
    procedure check parameter (filename : string);
    function f return integer;
end package;
package body p is
    procedure check parameter (filename : string) is begin null; end procedure;
    function f return integer is begin return 32D"1" + 32SB"1" + 16X"ff"; end function;
end package body;
"#;
    let (p, db) = parse_str(src);
    no_errors(&p, &db);
}

#[test]
fn parses_block_header_sized_bitstring_and_misc() {
    let src = r#"entity t is
    generic (g_w : natural := 32X"0" & "00");
end entity;
architecture a of t is
    signal d0, fz : bit := '0';
    disconnect bus_sig : bit after 3 ns;
    file f : text is "data.txt";
begin
    b : block is
            port ( z : out bit );
          port map ( z => fz );
    begin
        z <= d0;
    end block b;
    with d0 select (fz, d0) <= bit_vector'("00") when "0", "01" when others;
    p : alib.apkg.acomp port map (d0, fz);
    assert "<"(1, 2) = '1' report "no";
end architecture;
"#;
    let (p, db) = parse_str(src);
    no_errors(&p, &db);
    let sexp = p.root.debug_sexp(&db);
    assert!(sexp.contains("(PortClause"), "{}", sexp);
    assert!(sexp.contains("(DisconnectSpec"), "{}", sexp);
    assert!(sexp.contains("(FileDecl"), "{}", sexp);
    assert!(sexp.contains("(SelectedAssign"), "{}", sexp);
    assert!(sexp.contains("(Inst"), "{}", sexp);
    assert!(sexp.contains("(Call"), "{}", sexp); // "<"(1, 2)
                                                 // sized bit-string literal: size + base letter + string in one Literal
    assert!(
        sexp.contains("(Literal IntLiteral \"32\" Ident \"X\" StrLiteral"),
        "{}",
        sexp
    );
}
#[test]
fn parses_configuration_and_context_clause() {
    let sexp = ok(r#"context ieee.std_logic_1164.all;
configuration cfg of top is
  for rtl
    for u0 : counter
      use entity work.counter(rtl);
    end for;
  end for;
end configuration;
"#);
    assert!(sexp.contains("(ContextClause"), "{}", sexp);
    assert!(sexp.contains("(ConfigDecl"), "{}", sexp);
    assert_eq!(sexp.matches("(ForSpec").count(), 2, "{}", sexp);
}

#[test]
fn parses_package_instantiation() {
    let sexp = ok(r#"library ieee;
package p is new ieee.std_logic_1164.all generic map (W => 8);
"#);
    let _ = sexp;
    let sexp = ok(
        r#"package my_pkg is new work.generic_pkg generic map (W => 8);
"#,
    );
    assert!(sexp.contains("(PackageInst"), "{}", sexp);
}
#[test]
fn parses_vhdl2008_constructs() {
    // impure/pure functions, type generics + subprogram specs in a generic
    // clause, protected type + body, incomplete type, alias with [signature],
    // aggregate range choices, qualified aggregates, for-loop attribute
    // bounds, conditional expressions, parenthesized target list.
    let sexp = ok(r#"package p is
  generic (
    type ExpectedType;
    impure function Match(A : ExpectedType) return boolean
  );
  type ListType;
  type PType is protected
    procedure bump(x : in integer);
  end protected PType;
  alias F is g[integer, string];
end package;
package body p is
  type PType is protected body
    variable n : integer;
  end protected body PType;
  function f(a : integer_vector) return integer is
    variable v : integer;
  begin
    v := f(integer_vector'(1, 7)) when a'length > 0 else 0;
    for i in integer'left to integer'right loop
      v := v + i;
    end loop;
    return v;
  end function "+";
end package body;
architecture rtl of t is
  constant C : string := (1 to 4 => ' ');
  signal s : bit;
begin
  s <= '1' when C = "    " else '0';
  process
  begin
    (s, s) := f(integer_vector'(1, 2));
    wait;
  end process;
end architecture;
"#);
    assert!(sexp.contains("(GenericClause"), "{}", sexp);
    assert!(sexp.contains("(TypeDecl"), "{}", sexp);
    assert!(sexp.contains("(AliasDecl"), "{}", sexp);
    assert!(sexp.contains("(Qualified"), "{}", sexp);
}

#[test]
fn parses_generic_package_declaration() {
    let sexp = ok(r#"package gen_pkg is
  generic (WIDTH : natural := 8; DEPTH : positive := 4);
  constant C_W : natural := WIDTH;
end package;
package body gen_pkg is
begin
end package body;
architecture rtl of top is
  package inst_pkg is new work.gen_pkg generic map (WIDTH => 16);
  use work.inst_pkg.all;
begin
end architecture;
"#);
    // one GenericClause in the package declaration
    assert_eq!(sexp.matches("(GenericClause").count(), 1, "{}", sexp);
    assert!(sexp.contains("(PackageInst"), "{}", sexp);
    assert_eq!(sexp.matches("(UseClause").count(), 1, "{}", sexp);
}
#[test]
fn parses_rich_type_declarations() {
    let sexp = ok(r#"architecture rtl of m is
    type rec_t is record
        a : std_logic;
        b : integer range 0 to 3;
    end record;
    type ptr_t is access std_logic_vector(7 downto 0);
    type file_t is file of std_logic_vector;
    file f : file_t open read_mode is "in.txt";
    type prot_t is protected
        procedure p(x : in integer);
    end protected;
    type time_t is range 0 to 1000
        units
            fs;
            ps = 1000 fs;
            ns = 1000 ps;
        end units;
    subtype resolved_s is resolved std_logic;
    subtype custom_res is my_resolver std_logic;
    signal s : register std_logic;
    signal b : bus unsigned(3 downto 0);
    shared variable sv : integer;
    alias al : std_logic is s;
    attribute keep : boolean;
    attribute keep of s : signal is true;
    use work.pkg.all;
    for u0 : counter use entity work.counter(rtl);
begin
end;
"#);
    for node in [
        "(TypeDecl",
        "(FileDecl",
        "(AliasDecl",
        "(AttrDecl",
        "(AttrSpec",
        "(VariableDecl",
        "(ForSpec",
    ] {
        assert!(sexp.contains(node), "{} missing in {}", node, sexp);
    }
}
#[test]
fn parses_context_declaration() {
    let sexp = ok(r#"library ieee;
context OsvvmContext is
  library OSVVM ;
  use OSVVM.IfElsePkg.all ;
end context OsvvmContext;
entity e is end entity;
"#);
    assert!(sexp.contains("(ContextDecl"), "{}", sexp);
}

#[test]
fn parses_attribute_spec_with_signature() {
    let sexp = ok(r#"package p is
  impure function f(name: string) return integer;
  attribute foreign of f[string return integer]: function is "VHPI systf; f";
end package;
"#);
    assert!(sexp.contains("(AttrSpec"), "{}", sexp);
}
#[test]
fn parses_2019_generic_array_type_and_prefix_reduction() {
    let sexp = ok(r#"package p is
  generic (type VectorType is array (type is range <>) of type is private);
  subtype ElementType is VectorType'element;
end package;
package body p is
  function f(a : integer_vector) return bit is
  begin
    return (or a) and '1';
  end;
end package body;
"#);
    assert!(sexp.contains("Punct \"'\" Ident \"element\""), "{}", sexp);
    assert!(sexp.contains("(UnaryExpr Ident \"or\""), "{}", sexp);
}

#[test]
fn parses_block_configurations_and_specifications() {
    let sexp = ok(r#"configuration cfg of top is
  for rtl
    for u0 : comp
      use entity work.other(sim) generic map (W => 4) port map (a => x);
    end for;
    for all : leaf use configuration work.leaf_cfg;
    end for;
    for others : leaf use entity work.dflt(rtl);
    end for;
  end for;
end configuration;
entity e is end entity;
architecture rtl of e is
  for u0 : comp use entity work.other(sim);
begin
end architecture;
"#);
    assert!(sexp.contains("(ConfigDecl"), "{}", sexp);
    assert!(sexp.matches("(UseClause").count() >= 3, "{}", sexp);
    assert!(sexp.matches("(ForSpec").count() >= 5, "{}", sexp);
}
#[test]
fn parses_alias_with_signature_and_sized_signed_bitstring() {
    let sexp = ok(r#"package p is
    type text_file is file of string;
    function is_eof (file t : text_file; fake : boolean) return boolean;
    alias iseof is is_eof [text_file, boolean return boolean];
    constant c : bit_vector := 32SB"1010";
end package;
"#);
    assert!(sexp.contains("(AliasDecl"), "{}", sexp);
    assert!(
        sexp.contains("(Literal IntLiteral \"32\" Ident \"SB\" StrLiteral"),
        "{}",
        sexp
    );
}
#[test]
fn parses_vhdl2019_block_comments_views_and_external_names() {
    // VHDL-2019: `/* */` block comments, view declarations, `view` interface
    // mode (incl. parenthesized view name and nested view elements), and
    // external names with `^`/`.all` paths — as alias targets too.
    let sexp = ok(r#"package p is
  /* block
     comment */
  type sub_t is record
    addr : bit_vector(7 downto 0);
  end record;
  type rec_t is record
    wa : sub_t;
    wd : sub_t;
  end record;
  view subv of sub_t is
    addr : out;
  end view subv;
  view v1 of rec_t is
    wa : view subv;
    wd : in;
  end view v1;
  alias v2 is v1'converse;
end package;

entity e is
  port (
    bus : view work.p.v1 of work.p.rec_t;
    bus2 : view (v1) of rec_t
  );
end entity;

architecture rtl of e is
  alias ext is << signal ^.u0.internal_sig.all : bit_vector(3 downto 0) >>;
begin
end architecture;
"#);
    assert_eq!(sexp.matches("(ViewDecl").count(), 2, "{}", sexp);
    assert!(sexp.contains("(ExtName"), "{}", sexp);
    // The view-mode type mark is the subtype after `of`, not the view name.
    assert!(sexp.contains("Ident \"of\" (Name Ident \"work\" Punct \".\" Ident \"p\" Punct \".\" Ident \"rec_t\")"), "{}", sexp);
}
