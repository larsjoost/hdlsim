// Compiler directives: function-like macros with formal arguments, macro
// references in statement position, and nested macro expansion.
`define WIDTH 8
`define ADD(a, b) ((a) + (b))
`define SATINC(x) ADD(x, 4'd1)
`define CHECK(c) if (!(c)) begin $display("FAIL: macros"); $fatal(1); end
module tb;
  reg [`WIDTH-1:0] a, y;
  // A two-argument call as one actual: its comma must not split the
  // macro's argument list (LRM 19.3.1).
  function [`WIDTH-1:0] mix(input [`WIDTH-1:0] p, input [`WIDTH-1:0] q);
    mix = p + q;
  endfunction
  initial begin
    a = 4'd2;
    // Function-like macro with a based-literal actual.
    y = `ADD(a, 4'd1);
    `CHECK(y == 8'd3);
    // Macro expanding to another function-like macro.
    y = `SATINC(a);
    `CHECK(y == 8'd3);
    // Comma inside nested parens stays inside one actual.
    y = `ADD(mix(a, 4'd0), 4'd1);
    `CHECK(y == 8'd3);
    $display("PASS");
    $finish;
  end
endmodule
