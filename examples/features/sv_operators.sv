// Arithmetic, bitwise, logical, reduction, shift, ternary, comparison.
module tb;
  initial begin
    if (7 + 5 == 12 && 7 - 10 == -3 && 6 * 7 == 42 && 20 / 6 == 3 && 20 % 6 == 2)
      if ((8'hF0 & 8'h3C) == 8'h30 && (8'hF0 | 8'h3C) == 8'hFC && (8'hF0 ^ 8'hFF) == 8'h0F)
        if (~8'h0F == 8'hF0 && !8'h00 && (8'h80 >> 4) == 8'h08 && (8'h01 << 6) == 8'h40)
          if ((&8'hFF) && (!|8'h00) && (|8'h00) == 1'b0 && (^8'b1011) == 1'b1)
            if ((5 > 3 && 3 <= 3 && 2 != 4 && 1 == 1) && (4 > 2 ? 10 : 20) == 10)
              $display("PASS");
    if (1 == 0) $display("FAIL");
    $finish;
  end
endmodule
