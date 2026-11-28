//! Unit tests for the Verilog parser core: modules, ports, declarations,
//! generate regions, functions/tasks, gate primitives, typedefs, plus the
//! losslessness/recovery guarantees and the shared `parse_str`/`ok`
//! helpers. Child module of `verilog::parser` (house pattern: tests stay
//! colocated with the parser, split by grammar area).

use super::*;
use crate::source::SourceDb;

pub(super) fn parse_str(src: &str) -> (ParsedFile, SourceDb) {
    let mut db = SourceDb::new();
    let id = db.add_file("t.v", src).unwrap();
    (parse_file(id, db.file(id)), db)
}

pub(super) fn ok(src: &str) -> String {
    let (p, db) = parse_str(src);
    let errs: Vec<_> = p
        .diags
        .iter()
        .filter(|d| d.severity == crate::diagnostic::Severity::Error)
        .collect();
    assert!(
        errs.is_empty(),
        "{}\n{}",
        errs.iter()
            .map(|d| d.render(&db))
            .collect::<Vec<_>>()
            .join("\n"),
        src
    );
    assert_eq!(p.root.text(&db), src, "tree must be lossless");
    p.root.debug_sexp(&db)
}

#[test]
fn parses_wildcard_and_shorthand_port_conns() {
    let src = "module inner(input a, output y);
assign y = ~a;
endmodule
module tb;
reg a; wire y;
inner u1 (.*);
inner u2 (.a, .y);
endmodule
";
    let (p, db) = parse_str(src);
    assert!(
        !p.diags
            .iter()
            .any(|d| d.severity == crate::diagnostic::Severity::Error),
        "{:?}",
        p.diags.iter().map(|d| d.render(&db)).collect::<Vec<_>>()
    );
    let sexp = p.root.debug_sexp(&db);
    // The wildcard conn keeps its `.` and `*` punct children, and the
    // shorthand conn has a name but no expression.
    assert!(
        sexp.contains(r#"(PortConn Punct "." Punct "*")"#),
        "{}",
        sexp
    );
    assert!(sexp.contains(r#"(PortConn Punct "." (Name"#), "{}", sexp);
}

#[test]
fn parses_simple_module() {
    let (p, db) = parse_str("module m(input clk, output reg [7:0] q);\nendmodule\n");
    assert!(
        !p.diags
            .iter()
            .any(|d| d.severity == crate::diagnostic::Severity::Error),
        "{:?}",
        p.diags.iter().map(|d| d.render(&db)).collect::<Vec<_>>()
    );
    let sexp = p.root.debug_sexp(&db);
    assert!(sexp.contains("(Module"), "{}", sexp);
    assert!(sexp.contains("(Port"), "{}", sexp);
}

#[test]
fn parses_generate_for_and_case() {
    let src = r#"module g(input [3:0] sel, output reg y);
genvar i;
generate
  for (i = 0; i < 4; i = i + 1) begin : blk
    always @* y = sel[i];
  end
endgenerate
endmodule
"#;
    let (p, db) = parse_str(src);
    let errs: Vec<_> = p
        .diags
        .iter()
        .filter(|d| d.severity == crate::diagnostic::Severity::Error)
        .collect();
    assert!(
        errs.is_empty(),
        "{}",
        errs.iter()
            .map(|d| d.render(&db))
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(p.root.debug_sexp(&db).contains("(GenFor"));
}

#[test]
fn recovery_keeps_tree_lossless() {
    let src = "module m; wire ; endmodule\n";
    let (p, db) = parse_str(src);
    assert!(p
        .diags
        .iter()
        .any(|d| d.severity == crate::diagnostic::Severity::Error));
    assert_eq!(p.root.text(&db), src);
}

#[test]
fn never_panics_on_garbage() {
    for src in [
        "",
        "module",
        "module m",
        "(((((",
        "1+2*",
        "case case",
        "`timescale 1ns/1ps\nmodule m;endmodule",
        "always @* begin if end",
        "assign = = =;",
        "task",
        "fork",
        "fork begin end",
        "while (",
        "do",
        "->",
        "defparam",
        "typedef enum",
        "module m; and; endmodule",
        "module m; assert; endmodule",
        "module m; wait; endmodule",
    ] {
        let _ = parse_str(src);
    }
}

/// Parse and assert zero error diagnostics; returns the s-expression.

#[test]
fn parses_task_disable_and_wait() {
    let sexp = ok(r#"module m;
task automatic tsk(input [7:0] d, output reg [7:0] q);
  q = d;
endtask
initial begin
  wait (ready);
  tsk(.d(8'h01), .q(q));
  disable tsk;
end
endmodule
"#);
    assert!(sexp.contains("(Task"), "{}", sexp);
    assert!(sexp.contains("(Disable"), "{}", sexp);
    assert!(sexp.contains("(Wait"), "{}", sexp);
}

#[test]
fn parses_event_decl_and_triggers() {
    let sexp = ok(r#"module m;
event ev;
initial begin
  -> ev;
  ->> ev2;
end
endmodule
"#);
    assert!(sexp.contains("(EventDecl"), "{}", sexp);
    assert_eq!(sexp.matches("(Trigger").count(), 2, "{}", sexp);
}

#[test]
fn parses_defparam_and_final() {
    let sexp = ok(r#"module m;
  defparam u0.WIDTH = 16, u1.DEPTH = 4;
  final $display("done");
endmodule
"#);
    assert!(sexp.contains("(Defparam"), "{}", sexp);
    assert!(sexp.contains("(Final"), "{}", sexp);
}

#[test]
fn parses_generate_case_and_named_block() {
    let sexp = ok(r#"module m(input [1:0] sel);
generate
  case (sel)
    2'd0: begin : low
      assign y = a;
    end
    default: begin : high
      assign y = b;
    end
  endcase
endgenerate
endmodule
"#);
    assert!(sexp.contains("(GenCase"), "{}", sexp);
    assert_eq!(sexp.matches("(GenBlock").count(), 2, "{}", sexp);
}

#[test]
fn parses_gate_primitives_with_strength() {
    let sexp = ok(r#"module m(input a, b, output o);
  and (o, a, b);
  nand (strong1, pull0) g1 (n, a, b);
  pullup (o);
  bufif1 (t, a, en);
endmodule
"#);
    assert_eq!(sexp.matches("(Inst").count(), 4, "{}", sexp);
}

#[test]
fn parses_typedef_enum_and_struct() {
    let sexp = ok(r#"module m;
typedef enum logic [1:0] { IDLE = 0, RUN = 1, DONE = 2 } state_t;
typedef struct packed signed {
  logic [7:0] hi;
  logic [7:0] lo;
} pair_t;
typedef logic [15:0] word_t;
endmodule
"#);
    assert!(sexp.contains("(EnumType"), "{}", sexp);
    assert_eq!(sexp.matches("(StructType").count(), 1, "{}", sexp);
    assert_eq!(sexp.matches("(Typedef").count(), 3, "{}", sexp);
}

#[test]
fn parses_function_with_ansi_ports_and_param_type() {
    let sexp = ok(r#"module m;
  parameter type T = logic [7:0];
  function automatic logic [7:0] f(input logic [7:0] a, input int b);
    logic [7:0] tmp;
    tmp = a + b;
    return tmp;
  endfunction
endmodule
"#);
    assert!(sexp.contains("(Function"), "{}", sexp);
    assert!(sexp.contains("(Port"), "{}", sexp);
}

#[test]
fn parses_sv_variable_modifiers_and_types() {
    let sexp = ok(r#"module m;
  bit [3:0] nib;
  string s;
  var int v;
  const int C = 3;
  rand reg [7:0] r;
  randc int rc;
endmodule
"#);
    assert_eq!(sexp.matches("(Decl").count(), 6, "{}", sexp);
}

#[test]
fn parses_common_cells_constructs() {
    // Constructs from pulp-platform/common_cells: typed parameters,
    // user-typed ports/params, struct types, implicit generate regions,
    // inc/dec, compound assigns, size casts, multi-dim ranges, attributes,
    // labeled endmodule, and (dropped) macro invocations incl. multi-line.
    let sexp = ok(r#"module m #(
    parameter int unsigned Width = 1,
    parameter type data_t = logic
) (
    input  data_t d_i,
    output data_t d_o
);
  localparam cb_seed_t [2:0] Seeds = '{1'b0, 1'b1, 1'b0};
  typedef struct packed {
    int unsigned PermuteSeed;
    int unsigned XorSeed;
  } cb_seed_t;
  (* dont_touch = "true" *) logic async_req;
  data_t [Width-1:0] mem_n, mem_q;
  logic [Width-1:0][3:0] sum;
  if (Width > 1) begin : gen_wide
    assign d_o = d_i;
  end else begin : gen_narrow
    assign d_o = '0;
  end
  for (genvar j = 0; j < Width; j++) begin : gen_j
    always_comb begin
      sum[j] += data_t'(j);
      mem_q[j]++;
    end
  end
  `ASSERT(lock, req_o && !gnt_i,
    "multi line message")
endmodule : m
"#);
    assert!(sexp.contains("(ParamDecl"), "{}", sexp);
    assert!(sexp.contains("(StructType"), "{}", sexp);
    assert!(sexp.contains("(GenIf"), "{}", sexp);
    assert!(sexp.contains("(GenFor"), "{}", sexp);
}

#[test]
fn parses_escaped_identifiers() {
    let sexp = ok("module m; wire \\a+b ; assign \\a+b = 1'b0; endmodule\n");
    assert!(sexp.contains("\\a+b"), "{}", sexp);
}

#[test]
fn parses_module_header_package_imports() {
    // `module m import p::*; (...)` and `module m import p::*;` (no ports).
    let sexp = ok("module m import p::*; (input logic a); endmodule\n");
    assert!(sexp.contains("(Import"), "{}", sexp);
    let sexp = ok("module m import p::*, q::x; #(parameter W=8) (input logic a); endmodule\n");
    assert!(sexp.contains("(Import"), "{}", sexp);
    ok("module m import p::*; endmodule\n");
}

#[test]
fn parses_bare_enum_decl_and_multidim_ports() {
    let sexp = ok("module m(input logic [2:0][5:0] r, output logic [1:0] o); enum logic [1:0] { A, B } state, next; assign o = state; endmodule\n");
    assert!(sexp.contains("(EnumType"), "{}", sexp);
    assert!((sexp.matches("(Range").count() >= 3), "{}", sexp);
}
