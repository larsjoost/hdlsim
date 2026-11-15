// $urandom_range bounds and $rand_solve constraints.
module tb;
  integer i, v, a, b, ok;
  initial begin
    ok = 1;
    for (i = 0; i < 20; i = i + 1) begin
      v = $urandom_range(5, 9);
      if (v < 5 || v > 9) ok = 0;
    end
    $rand_solve("a: 0..10; b: 0..10; a + b == 10; a != b", "a", a, "b", b);
    if (ok && a + b == 10 && a != b) $display("PASS"); else $display("FAIL %0d %0d", a, b);
    $finish;
  end
endmodule
