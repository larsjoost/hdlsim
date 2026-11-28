//! Tests for the VHDL lowerer (`lower_file` entry): context-clause unit
//! collection, did-you-mean suggestions, and lowerer regressions (access
//! types, nested records via used packages, protected-type member selects,
//! package-level signals, parameter-dependent locals, …).
//!
//! Extracted verbatim from vhdl_lower.rs (pure move, no logic changes).

use super::*;

/// Levenshtein edit distance, counted in chars.
fn levenshtein(a: &str, b: &str) -> usize {
    crate::levenshtein_bounded(a, b, usize::MAX)
}

#[test]
fn context_libraries_collects_library_wide_uses() {
    let src = "library math;\nuse math.all;\nuse math.math.elog2;\nuse ieee.std_logic_1164.all;\nentity t is end;\n";
    let mut db = SourceDb::new();
    let id = db.add_file("t.vhd".to_string(), src.to_string()).unwrap();
    let parsed = hdl_syntax::vhdl::parser::parse_file(id, db.file(id));
    assert_eq!(
        context_libraries(&db, &parsed.root),
        vec!["math".to_string()]
    );
    // 3-part names still yield unit names, not libraries.
    assert!(context_units(&db, &parsed.root).contains(&"math".to_string()));
    assert!(context_units(&db, &parsed.root).contains(&"std_logic_1164".to_string()));
}

#[test]
fn did_you_mean_prefers_close_names() {
    let cands = ["edge_detector_entity".to_string(), "fifo".to_string()];
    // Prefix overlap: `edge_detector` is a prefix of `edge_detector_entity`.
    assert_eq!(
        did_you_mean("edge_detector", cands.iter()),
        Some("edge_detector_entity")
    );
    // Small edit distance.
    assert_eq!(did_you_mean("fifo2", cands.iter()), Some("fifo"));
    // Case-insensitive exact match is not suggested.
    assert_eq!(did_you_mean("FIFO", cands.iter()), None);
    // No close candidate at all.
    assert_eq!(did_you_mean("zzzzzz", cands.iter()), None);
    assert_eq!(did_you_mean("", cands.iter()), None);
}

#[test]
fn levenshtein_distance_counts_chars() {
    assert_eq!(levenshtein("kitten", "sitting"), 3);
    assert_eq!(levenshtein("abc", "abc"), 0);
    assert_eq!(levenshtein("", "ab"), 2);
}

#[test]
fn access_type_index_and_deref_lower_cleanly() {
    // `l(0).all` — index into an access-to-array auto-derefs, `.all`
    // derefs the element access; folded into one Name node by the parser.
    let src = "package p is\n  type line_t is access string;\n  type line_vec_t is array (natural range <>) of line_t;\n  type lines_t is access line_vec_t;\n  function mk return lines_t;\n  function g return string;\nend package;\npackage body p is\n  function mk return lines_t is\n    variable v : lines_t;\n  begin\n    return v;\n  end function;\n  function g return string is\n    variable l : lines_t := mk;\n  begin\n    return l(0).all;\n  end function;\nend package body;\n";
    let mut db = SourceDb::new();
    let id = db.add_file("t.vhd".to_string(), src.to_string()).unwrap();
    let parsed = hdl_syntax::vhdl::parser::parse_file(id, db.file(id));
    let mut diags = Vec::new();
    let _ = lower_file(
        id,
        &parsed.root,
        &db,
        &mut diags,
        1_000_000,
        &HashMap::new(),
        &std::rc::Rc::new(HashMap::new()),
        &HashMap::new(),
    );
    let errs: Vec<_> = diags
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(
        errs.is_empty(),
        "{:?}",
        errs.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
}

#[test]
fn nested_record_field_via_used_package_resolves() {
    // `m2s.ar.valid` — the record field type map must cross package
    // boundaries (PackageIr::rec_field_tys).
    let pkg = "package axi_pkg is\n  type ar_t is record\n    valid : bit;\n  end record;\n  type rd_t is record\n    ar : ar_t;\n  end record;\nend package;\n";
    let top = "use work.axi_pkg.all;\nentity top is\n  port (q : out bit);\nend entity;\narchitecture rtl of top is\n  signal m2s : rd_t;\nbegin\n  q <= m2s.ar.valid;\nend architecture;\n";
    let mut db = SourceDb::new();
    let id1 = db.add_file("p.vhd".to_string(), pkg.to_string()).unwrap();
    let parsed1 = hdl_syntax::vhdl::parser::parse_file(id1, db.file(id1));
    let mut diags = Vec::new();
    let mut pkgs = HashMap::new();
    let lowered = lower_file(
        id1,
        &parsed1.root,
        &db,
        &mut diags,
        1_000_000,
        &HashMap::new(),
        &std::rc::Rc::new(HashMap::new()),
        &HashMap::new(),
    );
    for p in lowered.packages {
        pkgs.insert(p.name.to_ascii_lowercase(), std::rc::Rc::new(p));
    }
    let id2 = db.add_file("top.vhd".to_string(), top.to_string()).unwrap();
    let parsed2 = hdl_syntax::vhdl::parser::parse_file(id2, db.file(id2));
    let _ = lower_file(
        id2,
        &parsed2.root,
        &db,
        &mut diags,
        1_000_000,
        &pkgs,
        &pkg_funcs_from(&pkgs),
        &HashMap::new(),
    );
    let errs: Vec<_> = diags
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(
        errs.is_empty(),
        "{:?}",
        errs.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
}

/// Design-wide const-eval fallback for tests, mirroring how hdl-lib
/// maintains `Design::pkg_funcs` as packages arrive.
fn pkg_funcs_from(
    pkgs: &HashMap<String, std::rc::Rc<PackageIr>>,
) -> std::rc::Rc<HashMap<String, Vec<std::rc::Rc<FuncIr>>>> {
    let mut m: HashMap<String, Vec<std::rc::Rc<FuncIr>>> = HashMap::new();
    for p in pkgs.values() {
        for f in &p.functions {
            m.entry(f.name.to_ascii_lowercase())
                .or_default()
                .push(std::rc::Rc::new(f.clone()));
        }
    }
    std::rc::Rc::new(m)
}

fn lower_src(src: &str) -> Vec<hdl_syntax::diagnostic::Diagnostic> {
    let mut db = SourceDb::new();
    let id = db.add_file("t.vhd".to_string(), src.to_string()).unwrap();
    let parsed = hdl_syntax::vhdl::parser::parse_file(id, db.file(id));
    let mut diags = Vec::new();
    let _ = lower_file(
        id,
        &parsed.root,
        &db,
        &mut diags,
        1_000_000,
        &HashMap::new(),
        &std::rc::Rc::new(HashMap::new()),
        &HashMap::new(),
    );
    diags
}

fn assert_no_errors(diags: &[hdl_syntax::diagnostic::Diagnostic]) {
    let errs: Vec<_> = diags
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(
        errs.is_empty(),
        "{:?}",
        errs.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
}

#[test]
fn process_local_function_bare_call_resolves() {
    // `function prefix return string` inside a process, called bare.
    let diags = lower_src("entity top is port (q : out bit); end entity;\narchitecture rtl of top is\nbegin\n  process\n    function prefix return string is\n    begin\n      return \"x\";\n    end function prefix;\n  begin\n    report prefix & \"done\";\n    wait;\n  end process;\nend architecture;\n");
    assert_no_errors(&diags);
}

#[test]
fn process_local_procedure_with_port_record_field_is_skipped() {
    // A process-local *procedure* (no `return`) whose body selects a field
    // of a record-typed entity port: procedures are not lowered (the
    // subprogram slot space does not see outer-scope names), so this must
    // not produce "unknown identifier" diagnostics.
    let diags = lower_src("package p is\n  type rec_t is record readdata : std_logic_vector; writedata : std_logic_vector; end record;\nend package;\nuse work.p.all;\nentity top is\n  port (amr : inout rec_t; rdata : in std_logic_vector(31 downto 0); wdata : out std_logic_vector(31 downto 0));\nend entity;\narchitecture rtl of top is\nbegin\n  process\n    procedure wr is\n    begin\n      amr.readdata <= rdata;\n      wdata <= amr.writedata;\n    end procedure;\n  begin\n    wr;\n    wait;\n  end process;\nend architecture;\n");
    assert_no_errors(&diags);
}

#[test]
fn record_param_field_in_call_arg_resolves() {
    // `get(q.p_meta, 0)` — field select on a record parameter inside a
    // constant initializer in the function's declarative part.
    let diags = lower_src("package p is\n  type inner_t is record v : integer; end record;\n  type q_t is record p_meta : inner_t; end record;\n  function get(x : integer; i : natural) return integer;\n  function get_meta(q : q_t) return integer;\nend package;\npackage body p is\n  function get(x : integer; i : natural) return integer is begin return x; end function;\n  function get_meta(q : q_t) return integer is\n    constant head : integer := get(q.p_meta, 0);\n  begin return head; end function;\nend package body;\n");
    assert_no_errors(&diags);
}

#[test]
fn protected_type_member_select_is_placeholder() {
    // `self.data_size` on a protected-typed shared variable.
    let diags = lower_src("package p is\n  type rec_t is protected\n    impure function get return natural;\n  end protected;\nend package;\npackage body p is\n  type rec_t is protected body\n    impure function get return natural is begin return 1; end;\n  end protected body;\nend package body;\nuse work.p.all;\nentity top is port (q : out bit); end entity;\narchitecture rtl of top is\n  shared variable self : rec_t;\nbegin\n  process begin\n    q <= bit'val(self.data_size > 0);\n    wait;\n  end process;\nend architecture;\n");
    assert_no_errors(&diags);
}

#[test]
fn record_typed_generic_field_select_resolves() {
    // `bus_handle.p_logger` where bus_handle is a record-typed generic.
    let diags = lower_src("package p is\n  type logger_t is record id : integer; end record;\n  type bus_t is record p_logger : logger_t; end record;\nend package;\nuse work.p.all;\nentity top is\n  generic (bus_handle : bus_t);\n  port (q : out bit);\nend entity;\narchitecture rtl of top is\nbegin\n  q <= bit'val(bus_handle.p_logger.id = 0);\nend architecture;\n");
    assert_no_errors(&diags);
}

#[test]
fn protected_pkg_shared_var_member_select_via_use_is_placeholder() {
    // `messenger.timeout_is_allowed` / `messenger.get_all_actors` where
    // messenger is a package-level shared variable of a protected type.
    let mut db = SourceDb::new();
    let id1 = db.add_file("p.vhd".to_string(), "package p is\n  type messenger_t is protected\n    impure function get_all_actors return integer;\n  end protected;\n  shared variable messenger : messenger_t;\nend package;\n".to_string()).unwrap();
    let parsed1 = hdl_syntax::vhdl::parser::parse_file(id1, db.file(id1));
    let mut diags = Vec::new();
    let mut pkgs = HashMap::new();
    let lowered = lower_file(
        id1,
        &parsed1.root,
        &db,
        &mut diags,
        1_000_000,
        &HashMap::new(),
        &std::rc::Rc::new(HashMap::new()),
        &HashMap::new(),
    );
    for p in lowered.packages {
        pkgs.insert(p.name.to_ascii_lowercase(), std::rc::Rc::new(p));
    }
    let id2 = db.add_file("top.vhd".to_string(), "use work.p.all;\npackage body p is\n  function f return boolean is\n    constant actors : integer := messenger.get_all_actors;\n  begin\n    return messenger.timeout_is_allowed or actors > 0;\n  end;\nend package body;\n".to_string()).unwrap();
    let parsed2 = hdl_syntax::vhdl::parser::parse_file(id2, db.file(id2));
    let _ = lower_file(
        id2,
        &parsed2.root,
        &db,
        &mut diags,
        1_000_000,
        &pkgs,
        &pkg_funcs_from(&pkgs),
        &HashMap::new(),
    );
    assert_no_errors(&diags);
}

#[test]
fn pkg_signal_alias_exported_and_usable_in_sens_list() {
    // `alias runner_phase is runner(...)` in a package with a
    // package-level signal: the alias must be visible in using files,
    // and the signal in a sensitivity list (`wait on runner`).
    let mut db = SourceDb::new();
    let id1 = db.add_file("p.vhd".to_string(), "package p is\n  signal runner : bit_vector(7 downto 0);\n  alias runner_phase is runner(3 downto 0);\nend package;\n".to_string()).unwrap();
    let parsed1 = hdl_syntax::vhdl::parser::parse_file(id1, db.file(id1));
    let mut diags = Vec::new();
    let mut pkgs = HashMap::new();
    let lowered = lower_file(
        id1,
        &parsed1.root,
        &db,
        &mut diags,
        1_000_000,
        &HashMap::new(),
        &std::rc::Rc::new(HashMap::new()),
        &HashMap::new(),
    );
    for p in lowered.packages {
        pkgs.insert(p.name.to_ascii_lowercase(), std::rc::Rc::new(p));
    }
    let id2 = db.add_file("top.vhd".to_string(), "use work.p.all;\nentity top is port (q : out bit); end entity;\narchitecture rtl of top is\nbegin\n  process\n  begin\n    wait on runner until runner_phase = \"0000\";\n    q <= '1';\n    wait;\n  end process;\nend architecture;\n".to_string()).unwrap();
    let parsed2 = hdl_syntax::vhdl::parser::parse_file(id2, db.file(id2));
    let _ = lower_file(
        id2,
        &parsed2.root,
        &db,
        &mut diags,
        1_000_000,
        &pkgs,
        &pkg_funcs_from(&pkgs),
        &HashMap::new(),
    );
    assert_no_errors(&diags);
}

#[test]
fn nested_function_referenced_in_constant_initializer() {
    // `constant result : string := create_string;` where create_string is
    // a function nested in the same subprogram (stub pre-registration).
    let diags = lower_src("package p is\nend package;\npackage body p is\n  function outer(s : string) return string is\n    impure function create_string return string is\n    begin\n      return s;\n    end;\n    constant result : string := create_string;\n  begin\n    return result;\n  end;\nend package body;\n");
    assert_no_errors(&diags);
}

#[test]
fn block_level_function_bare_call_resolves() {
    // `impure function next_outstanding ...` declared in a block's
    // declarative part, called bare from a process inside the block.
    let diags = lower_src("entity top is port (clk : in bit; q : out natural); end entity;\narchitecture a of top is\n  signal outstanding : natural := 0;\nbegin\n  write_block : block\n    impure function next_outstanding return natural is\n    begin\n      return outstanding + 1;\n    end;\n  begin\n    main : process\n    begin\n      wait until rising_edge(clk);\n      outstanding <= next_outstanding;\n      q <= outstanding;\n    end process;\n  end block;\nend architecture;\n");
    assert_no_errors(&diags);
}

#[test]
fn param_dependent_local_function_lowers_without_warning() {
    // `variable l : bit_vector(a'length - 1 downto 0);` inside a function
    // (as in Vivado's std_logic_arith): the function must still lower and
    // no "parameter-dependent" warning may be emitted (issue #106).
    let diags = lower_src("package p is\n  function f(a : bit_vector) return bit_vector;\nend package;\npackage body p is\n  function f(a : bit_vector) return bit_vector is\n    variable l : bit_vector(a'length - 1 downto 0);\n  begin\n    l := a;\n    return l;\n  end;\nend package body;\n");
    assert_no_errors(&diags);
    let msgs: Vec<&str> = diags.iter().map(|d| d.message.as_str()).collect();
    assert!(
        msgs.iter().all(|m| !m.contains("parameter-dependent")),
        "{msgs:?}"
    );
}

#[test]
fn loop_built_vector_in_constant_initializer_const_evals() {
    // `constant c := ones_vec(3);` where ones_vec fills the vector in a
    // for loop: the const evaluator must splice indexed assignments into
    // the local slot — this was the last remaining W0401 from the issue
    // #106 report (a testbench `constant c := generate_test_vector;`).
    let diags = lower_src("package p is\n  function ones_vec(n : integer) return bit_vector;\nend package;\npackage body p is\n  function ones_vec(n : integer) return bit_vector is\n    variable v : bit_vector(0 to n - 1);\n  begin\n    for i in 0 to n - 1 loop\n      v(i) := '1';\n    end loop;\n    return v;\n  end function;\nend package body;\nentity e is end entity;\narchitecture a of e is\n  constant c : bit_vector := work.p.ones_vec(3);\n  signal s : bit := c(0);\nbegin\nend architecture;\n");
    let msgs: Vec<String> = diags.iter().map(|d| d.message.clone()).collect();
    assert!(diags.is_empty(), "{msgs:?}");
}

#[test]
fn param_dependent_local_constants_lower_without_warning() {
    // std_logic_arith-style function-local constants (issue #106
    // follow-up): `constant rMsb : integer := SIZE - 1;` and
    // `constant msb : integer := min(ARG'length, SIZE) - 1;` are valid
    // VHDL. They lower as per-call runtime slots (initializer evaluated
    // on every call), so no "not constant; treated as X" warning.
    let diags = lower_src("package p is\n  type nat_mat is array (natural range <>, natural range <>) of natural;\n  function f(size : integer; m : nat_mat) return integer;\nend package;\npackage body p is\n  function f(size : integer; m : nat_mat) return integer is\n    constant rmsb : integer := size - 1;\n    constant msb  : integer := m'length(1) - size;\n    constant xsz  : integer := m'length(2);\n  begin\n    return rmsb + msb + xsz;\n  end function;\nend package body;\n");
    let msgs: Vec<String> = diags.iter().map(|d| d.message.clone()).collect();
    assert!(diags.is_empty(), "{msgs:?}");
}

#[test]
fn qualified_expression_in_concat_and_aggregate_form() {
    // `'0' & signed'(abs(b))` (Vivado std_logic_arith mult): the qualified
    // conversion is a plain operand of `&`; a qualified aggregate
    // `t'(field => v, others => x)` is a placeholder, not an error.
    let diags = lower_src("entity top is end entity;\narchitecture rtl of top is\n  signal b : bit_vector(7 downto 0) := (others => '0');\n  signal x : bit_vector(15 downto 0);\n  type rec_t is record elem : bit_vector(2 downto 0); end record;\n  signal r : rec_t;\nbegin\n  x <= '0' & bit_vector'(b);\n  r <= rec_t'(elem => \"000\");\n  r <= rec_t'(others => '0');\nend architecture;\n");
    assert_no_errors(&diags);
}

#[test]
fn real_literal_with_digit_separators() {
    // math_real.vhdl: `constant MATH_E : real := 2.71828_18284_59045_23536;`
    let diags = lower_src("package p is\n  constant math_e : real := 2.71828_18284_59045_23536;\n  constant math_pi : real := 3.14159_26535_89793_23846;\nend package;\n");
    assert_no_errors(&diags);
}

#[test]
fn package_level_signal_bare_read_via_use() {
    // `signal net : network_t` in a package; bare `net` in a using file.
    let mut db = SourceDb::new();
    let id1 = db.add_file("p.vhd".to_string(), "package p is\n  type network_t is record id : integer; end record;\n  signal net : network_t;\nend package;\n".to_string()).unwrap();
    let parsed1 = hdl_syntax::vhdl::parser::parse_file(id1, db.file(id1));
    let mut diags = Vec::new();
    let mut pkgs = HashMap::new();
    let lowered = lower_file(
        id1,
        &parsed1.root,
        &db,
        &mut diags,
        1_000_000,
        &HashMap::new(),
        &std::rc::Rc::new(HashMap::new()),
        &HashMap::new(),
    );
    for p in lowered.packages {
        pkgs.insert(p.name.to_ascii_lowercase(), std::rc::Rc::new(p));
    }
    let id2 = db.add_file("top.vhd".to_string(), "use work.p.all;\nentity top is port (q : out bit); end entity;\narchitecture rtl of top is\nbegin\n  q <= bit'val(net.id = 0);\nend architecture;\n".to_string()).unwrap();
    let parsed2 = hdl_syntax::vhdl::parser::parse_file(id2, db.file(id2));
    let _ = lower_file(
        id2,
        &parsed2.root,
        &db,
        &mut diags,
        1_000_000,
        &pkgs,
        &pkg_funcs_from(&pkgs),
        &HashMap::new(),
    );
    assert_no_errors(&diags);
}

#[test]
fn invalid_generate_range_and_literal_base_do_not_panic() {
    // Regression: a for-generate over a real range (`0.0 to 8.0`) read the
    // f64 bit pattern as a huge u64 and overflowed the unroll Vec; a based
    // literal with base 0 (`0#121#`) panicked in from_str_radix.
    for src in [
        "entity e is end;\narchitecture a of e is\nbegin\n  g : for i in 0.0 to 8.0 generate\n    null;\n  end generate g;\nend;\n",
        "entity e is end;\narchitecture a of e is\nbegin\n  process\n    variable i : integer;\n  begin\n    i := 0#121#E2;\n    wait;\n  end process;\nend;\n",
    ] {
        let mut db = SourceDb::new();
        let id = db.add_file("t.vhd".to_string(), src.to_string()).unwrap();
        let parsed = hdl_syntax::vhdl::parser::parse_file(id, db.file(id));
        let mut diags = Vec::new();
        let _ = lower_file(
            id,
            &parsed.root,
            &db,
            &mut diags,
            1_000_000,
            &HashMap::new(),
            &std::rc::Rc::new(HashMap::new()),
            &HashMap::new(),
        );
    }
}
