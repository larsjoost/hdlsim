// always_ff on posedge clock, non-blocking updates, clock generation.
module tb;
  logic clk = 0;
  logic [7:0] q = 0;
  logic [7:0] d = 0;
  always_ff @(posedge clk) q <= q + 1;
  always @(posedge clk) d <= q;
  always #5 clk = ~clk;
  initial begin
    #12 if (q == 1 && d == 0) $display("PASS"); else $display("FAIL q=%0d d=%0d", q, d);
    #10 if (q == 2 && d == 1) $display("PASS"); else $display("FAIL q=%0d d=%0d", q, d);
    #10 $finish;
  end
endmodule
