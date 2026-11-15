// for / while / repeat / do-while / forever+break / continue.
module tb;
  integer i, s, w, r, d, f;
  initial begin
    int tmp;
    tmp = 7;
    s = tmp + 3;
    for (i = 0; i < 5; i = i + 1) s = s + i;
    w = 0;
    while (w < 5) w = w + 1;
    r = 0;
    repeat (4) r = r + 1;
    d = 0;
    do d = d + 1; while (d < 3);
    f = 0;
    forever begin
      f = f + 1;
      if (f == 3) break;
    end
    if (s == 20 && w == 5 && r == 4 && d == 3 && f == 3) begin
      s = 0;
      for (i = 0; i < 10; i = i + 1) begin
        if (i % 2 == 0) continue;
        s = s + 1;
      end
      if (s == 5) $display("PASS"); else $display("FAIL cont %0d", s);
    end else $display("FAIL %0d %0d %0d %0d %0d", s, w, r, d, f);
    $finish;
  end
endmodule
