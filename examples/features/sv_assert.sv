// Immediate assertion that holds, plus $display formatting.
module tb;
  initial begin
    assert (2 + 2 == 4) else $display("FAIL assert");
    $display("val=%0d hex=%h bin=%b", 42, 8'hA5, 4'b1011); $display("PASS");
    $finish;
  end
endmodule
