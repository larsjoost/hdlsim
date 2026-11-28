// SV time literals in delays: `#1ns`, `#(2*1ns)`, `#(1 ns)` (SV LRM 5.7.1).
// Each delays its stated absolute time: the literal is scaled into the
// module's time units (default `timescale` unit here is 1 ns), so `#1ns` is
// one unit and `#1us` is 1000 units. Before time-literal support `#1ns`
// mis-parsed as a declaration (`1` delay, then `ns q = 1`).
module tb;
  logic [7:0] q = 0;
  integer fails = 0;
  initial begin
    #1ns q = 8'h11; // t = 1 ns
    if (q !== 8'h11) begin $display("FAIL step1 q=%0d", q); fails = fails + 1; end
    #(2*1ns) q = 8'h22; // t = 3 ns
    if (q !== 8'h22) begin $display("FAIL step2 q=%0d", q); fails = fails + 1; end
    #(1 ns) q = 8'h33; // t = 4 ns
    if (q !== 8'h33) begin $display("FAIL step3 q=%0d", q); fails = fails + 1; end
    #1us q = 8'h44; // t = 1004 ns
    if (q !== 8'h44) begin $display("FAIL step4 q=%0d", q); fails = fails + 1; end
    // $time is femtoseconds in this simulator: 4 ns of delays + 1 us = 1.004 us.
    if ($time != 1004000000) begin $display("FAIL time=%0d", $time); fails = fails + 1; end
    if (fails == 0) $display("PASS"); else $display("FAIL %0d check(s)", fails);
    $finish;
  end
endmodule
