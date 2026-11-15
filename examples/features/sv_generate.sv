// generate for / if, parameters, hierarchical refs.
module adder #(parameter W = 8) (input wire [W-1:0] a, input wire [W-1:0] b, output wire [W-1:0] s);
  assign s = a + b;
endmodule
module tb;
  wire [7:0] s0, s1;
  adder #(.W(8)) u0 (.a(8'd10), .b(8'd20), .s(s0));
  adder u1 (.a(8'd1), .b(8'd2), .s(s1));
  genvar i;
  wire [3:0] bits;
  wire [7:0] extra;
  generate
    for (i = 0; i < 4; i = i + 1) begin : g
      assign bits[i] = i[0];
    end
    if (1) begin : g2
      assign extra = 8'h07;
    end
  endgenerate
  initial begin
    #1;
    if (s0 == 8'd30 && s1 == 8'd3 && bits == 4'b1010 && tb.g2.extra == 8'h07)
      $display("PASS");
    else $display("FAIL %0d %0d %b", s0, s1, bits);
    $finish;
  end
endmodule
