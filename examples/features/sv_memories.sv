// Memories, indexed read/write, part-selects, concat, replication.
module tb;
  reg [7:0] m [0:3];
  reg [7:0] a, x;
  reg [3:0] hi;
  reg [15:0] cat, rep;
  initial begin
    m[0] = 8'h11; m[1] = 8'h22; m[2] = 8'hAB; m[3] = 8'h00;
    x = m[2];
    a = 8'hF0;
    hi = a[7:4];
    cat = {a, 8'h3C};
    rep = {4{4'h5}};
    if (x == 8'hAB && hi == 4'hF && m[1] == 8'h22)
      if (cat == 16'hF03C && rep == 16'h5555) $display("PASS"); else $display("FAIL");
    $finish;
  end
endmodule
