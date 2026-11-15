// case / casez / casex / unique with default.
module tb;
  logic [3:0] v;
  logic [1:0] op;
  integer r;
  initial begin
    r = 0;
    v = 4'b1z01;
    casez (v)
      4'b1z01: r = r + 1;
      default: r = r + 100;
    endcase
    v = 4'b1x10;
    casex (v)
      4'b1x10: r = r + 1;
      default: r = r + 100;
    endcase
    v = 4'd7;
    case (v)
      4'd7: r = r + 1;
      default: r = r + 100;
    endcase
    op = 2'b01;
    unique case (op)
      2'b00: r = r + 100;
      2'b01: r = r + 1;
      default: r = r + 100;
    endcase
    if (r == 4) $display("PASS"); else $display("FAIL %0d", r);
    $finish;
  end
endmodule
