// Delays, edge waits, @*, non-blocking ordering, $display formats.
module tb;
  logic clk = 0;
  logic [7:0] q = 0;
  logic [7:0] qa, qb;
  always #5 clk = ~clk;
  always @(posedge clk) begin
    qa <= q;
    qb <= qa;
    q <= q + 1;
  end
  initial begin
    $display("t=%0t", $time);
    @(posedge clk);
    @(posedge clk);
    #1;
    if (q == 2 && qa == 1 && qb == 0) $display("PASS"); else $display("FAIL q=%0d qa=%0d qb=%0d", q, qa, qb);
    $finish;
  end
endmodule
