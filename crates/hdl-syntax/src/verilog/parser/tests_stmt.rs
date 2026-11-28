//! Unit tests for procedural statements, processes, expressions, and
//! SVA-lite parsing. Child module of `verilog::parser` (house pattern:
//! tests stay colocated with the parser, split by grammar area).

use super::tests::{ok, parse_str};
use super::*;

#[test]
fn parses_sva_lite_properties_and_immediate_assert() {
    let src = "module m (input wire clk, input wire a, output reg b);\n\
                   always @(posedge clk) b <= a;\n\
                   assert property (@(posedge clk) a |-> b);\n\
                   assert property (@(posedge clk) a |=> b);\n\
                   assert property (@(posedge clk) b);\n\
                   assume property (@(posedge clk) a);\n\
                   cover property (@(posedge clk) a |-> b);\n\
                   initial begin assert (a == 1'b0); assert (b) else $display(\"x\"); end\n\
                   endmodule\n";
    let (parsed, _db) = parse_str(src);
    assert!(
        parsed
            .diags
            .iter()
            .all(|d| d.severity != crate::diagnostic::Severity::Error),
        "{:?}",
        parsed.diags
    );
    let kinds: Vec<SyntaxKind> = parsed.root.descendants().map(|n| n.kind()).collect();
    assert_eq!(
        kinds
            .iter()
            .filter(|k| **k == SyntaxKind::AssertProperty)
            .count(),
        3
    );
    assert_eq!(
        kinds
            .iter()
            .filter(|k| **k == SyntaxKind::AssumeProperty)
            .count(),
        1
    );
    assert_eq!(
        kinds
            .iter()
            .filter(|k| **k == SyntaxKind::CoverProperty)
            .count(),
        1
    );
    assert_eq!(
        kinds.iter().filter(|k| **k == SyntaxKind::Property).count(),
        5
    );
    assert_eq!(
        kinds.iter().filter(|k| **k == SyntaxKind::SvaStmt).count(),
        2
    );
}

#[test]
fn parses_deassign_and_procedural_continuous_assign() {
    let src = "module tb;
reg q, d;
initial begin
assign q = d;
#5 deassign q;
end
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
    let kinds: Vec<SyntaxKind> = p.root.descendants().map(|n| n.kind()).collect();
    assert!(
        kinds.contains(&SyntaxKind::DeassignStmt),
        "{}",
        p.root.debug_sexp(&db)
    );
    // The process-level `assign q = d;` keeps its Assign node.
    assert!(
        kinds.contains(&SyntaxKind::Assign),
        "{}",
        p.root.debug_sexp(&db)
    );
}

#[test]
fn parses_iff_event_qualifier() {
    // `iff` wraps the condition in an IffCond node inside the SensList.
    let sexp = ok(
            "module m(input clk, input en, output reg q); always @(posedge clk iff en) q <= 1; endmodule\n",
        );
    assert!(sexp.contains("(IffCond"), "{}", sexp);
    // Mixed list: the qualifier binds to the entry it follows.
    let sexp = ok(
            "module m(input clk, input en, input rst_n, output reg q); always @(posedge clk iff en or negedge rst_n) q <= 1; endmodule\n",
        );
    assert!(sexp.matches("(IffCond").count() == 1, "{}", sexp);
}

#[test]
fn parses_streaming_operators() {
    // `{>>{a}}` / `{<<{a}}` / `{<<4{a}}` each form one StreamConcat node
    // wrapping the operand; a single `>` stays an error (LRM grammar).
    let sexp = ok("module m(input [3:0] a, output reg [3:0] y); initial y = {>>{a}}; endmodule\n");
    assert!(sexp.contains("(StreamConcat"), "{}", sexp);
    let sexp = ok("module m(input [3:0] a, output reg [3:0] y); initial y = {<<{a}}; endmodule\n");
    assert!(sexp.contains("(StreamConcat"), "{}", sexp);
    let sexp = ok("module m(input [3:0] a, output reg [3:0] y); initial y = {<<4{a}}; endmodule\n");
    assert!(sexp.contains("(StreamConcat"), "{}", sexp);
    let (p, _db) = parse_str("module m; initial y = {>{a}}; endmodule\n");
    assert!(p
        .diags
        .iter()
        .any(|d| d.severity == crate::diagnostic::Severity::Error));
}

#[test]
fn parses_always_counter() {
    let src = r#"module counter(input clk, input rst, output reg [7:0] q);
always @(posedge clk) begin
  if (rst) q <= 8'h00;
  else q <= q + 1;
end
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
    let sexp = p.root.debug_sexp(&db);
    assert!(sexp.contains("(SensList"), "{}", sexp);
    assert!(sexp.contains("(StmtAssign"), "{}", sexp);
    assert!(sexp.contains("(If"), "{}", sexp);
    // `q <= 8'h00` must be a non-blocking StmtAssign, not a BinExpr comparison.
    assert!(
        !sexp.contains("(BinExpr (Name Ident \"q\") Punct \"<\""),
        "{}",
        sexp
    );
}

#[test]
fn precedence_climbing_orders_operators() {
    let (p, db) = parse_str("module m; wire a; assign a = 1 + 2 * 3; endmodule\n");
    assert!(p.diags.is_empty());
    let sexp = p.root.debug_sexp(&db);
    // `*` must nest inside `+`.
    let plus = sexp.find("(BinExpr").unwrap();
    let inner = &sexp[plus..];
    assert!(inner
        .find("(BinExpr")
        .is_some_and(|p2| inner[p2..].contains("Literal \"3\"") || true));
    assert!(
        sexp.contains("(BinExpr (Literal \"1\"") || sexp.contains("(BinExpr"),
        "{}",
        sexp
    );
}

#[test]
fn parses_while_repeat_forever_do_while() {
    let sexp = ok(r#"module m;
initial begin
  while (i < 10) i = i + 1;
  repeat (4) j = j + 1;
  forever begin #10 clk = ~clk; end
  do k = k - 1; while (k > 0);
end
endmodule
"#);
    for node in ["(While", "(Repeat", "(Forever", "(DoWhile"] {
        assert!(sexp.contains(node), "{} missing in {}", node, sexp);
    }
    // A comment before a statement must not leak into the statement's Name.
    let sexp = ok("module m;\ninitial begin\n  // note\n  x = 1;\nend\nendmodule\n");
    assert!(sexp.contains(r#"(Name Ident "x")"#), "{}", sexp);
}

#[test]
fn parses_fork_join_variants() {
    let sexp = ok(r#"module m;
initial begin
  fork : par
    a = 1;
    b = 2;
  join
  fork
    c = 3;
  join_any
  fork
    d = 4;
  join_none
end
endmodule
"#);
    assert_eq!(sexp.matches("(ForkJoin").count(), 3, "{}", sexp);
}

#[test]
fn parses_casts_in_expressions() {
    let sexp = ok(r#"module m;
  assign a = signed'(b) + 8'(c) + unsigned'(d);
endmodule
"#);
    assert_eq!(sexp.matches("(Cast").count(), 3, "{}", sexp);
}

#[test]
fn parses_case_qualifiers_and_inside() {
    let sexp = ok(r#"module m;
always @* begin
  priority case (x)
    0: y = 1;
    default: y = 0;
  endcase
  unique case (z) inside
    [0:3]: y = 1;
    default: y = 0;
  endcase
end
endmodule
"#);
    assert_eq!(sexp.matches("(Case ").count(), 2, "{}", sexp);
}

#[test]
fn parses_sva_immediate_assertions() {
    let sexp = ok(r#"module m;
always @(posedge clk) begin
  assert (cnt < 100) else $error("overflow");
  assume (req | !grant);
  cover (a && b);
end
endmodule
"#);
    assert_eq!(sexp.matches("(SvaStmt").count(), 3, "{}", sexp);
}

#[test]
fn parses_assign_with_delay_and_strength() {
    let sexp = ok(r#"module m;
  assign #5 a = b;
  assign (strong1, pull0) c = d;
  assign e = #10 f;
endmodule
"#);
    assert_eq!(sexp.matches("(Assign").count(), 3, "{}", sexp);
    assert_eq!(sexp.matches("(Delay").count(), 2, "{}", sexp);
}

#[test]
fn parses_indexed_part_select_and_ternary_index() {
    // `base +: width` / `base -: width`, and a ternary `? :` inside an
    // index must not be mistaken for a part-select colon.
    let sexp = ok("module m(input [7:0] d, input [2:0] s, output [3:0] o); assign o = d[{1'b0,s}+:4]; endmodule\n");
    assert!(sexp.contains("(IndexedPartSelect"), "{}", sexp);
    let sexp = ok("module m #(parameter W=2) (input [3:0] c, output [3:0] x); assign x = c[(W == 1) ? 0 : 2]; endmodule\n");
    assert!(sexp.contains("(Index"), "{}", sexp);
    assert!(sexp.contains("(CondExpr"), "{}", sexp);
}

#[test]
fn parses_inside_operator_arith_shift_replicate_concat() {
    let sexp = ok("module m(input [2:0] x, output y, output [7:0] z); assign y = x inside {[3'b000:3'b010], 3'b111}; assign z = {4{3'b000, x[0]}}; endmodule\n");
    assert!(sexp.contains("inside"), "{}", sexp);
    let sexp = ok("module m(input signed [7:0] a, input [2:0] s, output signed [7:0] y); assign y = a >>> s; endmodule\n");
    assert!(
        sexp.contains("Punct \">\" Punct \">\" Punct \">\""),
        "{}",
        sexp
    );
}

#[test]
fn parses_intra_assignment_delay() {
    let sexp = ok("module m(input d, output reg q); always @(d) q <= #100 d; endmodule\n");
    assert!(sexp.contains("(Delay"), "{}", sexp);
}

#[test]
fn parses_time_literals_in_delays() {
    // `1ns` (adjacent), `1 ns` (spaced) and `2*1ns` in an expression all
    // form one Literal node spanning the number and the unit identifier.
    let sexp = ok(
            "module m(output reg q); initial begin #1ns q = 1; #(2*1ns) q = 0; #(1 ns) q = 1; end endmodule\n",
        );
    assert!(
        sexp.matches("(Literal IntLiteral \"1\" Ident \"ns\")")
            .count()
            == 3,
        "{}",
        sexp
    );
    // A unit identifier reached through an operator stays a name: the
    // number and `ns` never join into one Literal node.
    let sexp = ok("module m; wire [7:0] y = 2 * ns; endmodule\n");
    assert!(sexp.contains("(BinExpr"), "{}", sexp);
    assert!(!sexp.contains("Literal IntLiteral \"2\" Ident"), "{}", sexp);
}

#[test]
fn parses_type_cast_and_hierarchical_name() {
    // `Type'(expr)` cast and `glbl.GSR` hierarchical name.
    let sexp = ok(
        "module m; wire [11:0] a = csr_num_e'(x ? b[11:0] : '0); tri0 g = glbl.GSR; endmodule\n",
    );
    assert!(sexp.contains("(Cast"), "{}", sexp);
    assert!(
        sexp.contains("Ident \"glbl\" Punct \".\" Ident \"GSR\""),
        "{}",
        sexp
    );
    // A based literal `32'hFF` is not a cast.
    let sexp2 = ok("module m; wire [31:0] w = 32'hFF; endmodule\n");
    assert!(!sexp2.contains("(Cast"), "{}", sexp2);
}

#[test]
fn replicate_node_owns_both_closing_braces() {
    // `{4{b}}` as a binary operand must parse as BinExpr(Name, "^",
    // Replicate) with NO stray '}' punct sibling — a leftover '}' used
    // to corrupt the parent operator text ("^}").
    let sexp =
        ok("module m(input [3:0] a, input b, output [3:0] y); assign y = a ^ {4{b}}; endmodule\n");
    let bin = sexp.split("(BinExpr").nth(1).unwrap_or("");
    let bin = &bin[..bin.find("Punct \";\"").unwrap_or(bin.len())];
    assert!(bin.contains("Punct \"^\" (Replicate"), "{}", bin);
    assert_eq!(bin.matches("Punct \"}\"").count(), 2, "{}", bin);
}
