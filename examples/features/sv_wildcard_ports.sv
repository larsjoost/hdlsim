// SV-style implicit port connections: `.*` binds every port not named
// explicitly to the same-named signal in the enclosing scope, and the `.name`
// shorthand does the same for a single port (SV LRM 23.3.2.4).
// u1 connects all four ports via the wildcard, u2 overrides .a/.b explicitly
// (named connections win over .*), and u3 uses only the .name shorthand.
module adder #(
    parameter W = 8
) (
    input  wire [W-1:0] a,
    input  wire [W-1:0] b,
    output wire [W-1:0] sum,
    output wire         zero
);
  assign sum  = a + b;
  assign zero = (sum == {W{1'b0}});
endmodule

module tb;
  reg  [7:0] a, b;
  wire [7:0] sum, sum2;
  wire       zero, zero2;
  adder #(.W(8)) u1 (.*);
  adder #(.W(8)) u2 (.a(a), .b(b), .*);
  adder #(.W(8)) u3 (.a, .b, .sum(sum2), .zero(zero2));

  initial begin
    a = 8'd100;
    b = 8'd55;
    #5;
    if (sum == 8'd155 && zero == 1'b0 && sum2 == 8'd155 && zero2 == 1'b0) begin
      a = 8'd201;
      b = 8'd55;
      #5;
      if (sum == 8'd0 && zero == 1'b1 && sum2 == 8'd0 && zero2 == 1'b1) begin
        $display("PASS");
        $finish;
      end
    end
    $display("FAIL sum=%0d zero=%b sum2=%0d zero2=%b", sum, zero, sum2, zero2);
    $fatal(1);
  end
endmodule
