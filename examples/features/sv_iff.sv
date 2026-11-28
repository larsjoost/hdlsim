// SV `iff` event qualifier (SV LRM 9.4.2.3): `@(posedge clk iff en)` fires
// only when `en` is true at the clock edge — a gated clock sample. The
// condition is evaluated at each edge; an edge while `en` is false is
// invisible to the process. The qualifier binds to the entry it follows, so
// mixed lists keep other edges unconditional (`or negedge rst_n`).
module tb;
  reg clk = 0, en = 0, rst_n = 1;
  reg [1:0] q = 0;
  integer fails = 0;

  // q counts qualifying posedges; rst_n is an unconditional second arm.
  always @(posedge clk iff en or negedge rst_n) q = q + 1;

  always #10 clk = ~clk; // posedges at 10, 30, 50, 70 ns

  task check(input [1:0] expect_q, input [127:0] label);
    if (q !== expect_q) begin
      $display("FAIL %0s: q=%0d expected %0d", label, q, expect_q);
      fails = fails + 1;
    end
  endtask

  initial begin
    #15 en = 1;
    #20 check(1, "edge at t=30"); // posedge at t=30, en=1 -> fires
    en = 0;
    #20 check(1, "edge at t=50"); // posedge at t=50, en=0 -> gated
    en = 1;
    #20 check(2, "edge at t=70"); // posedge at t=70, en=1 -> fires
    rst_n = 0;
    #1 check(3, "negedge rst_n"); // unconditional arm fires immediately
    if (fails == 0) $display("PASS"); else $display("FAIL %0d check(s)", fails);
    $finish;
  end
endmodule
