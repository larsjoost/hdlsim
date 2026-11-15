// typedefs: plain, enum with labels, packed struct + assignment pattern.
module tb;
  typedef logic [7:0] byte_t;
  typedef enum logic [1:0] {IDLE, RUN, DONE} state_t;
  typedef struct packed { logic [7:0] a; logic [3:0] b; } pair_t;
  byte_t v;
  state_t s;
  pair_t p;
  initial begin
    v = 8'h2A;
    s = RUN;
    p = '{a: 8'h3C, b: 4'h5};
    if (v == 8'h2A && s == RUN && s != IDLE && p.a == 8'h3C && p.b == 4'h5)
      $display("PASS");
    else $display("FAIL");
    $finish;
  end
endmodule
