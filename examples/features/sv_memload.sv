// Memory load/store tasks (LRM 21.4): $writememh/$writememb write a memory
// image, $readmemh/$readmemb load it back. Covers the full-image round trip,
// the optional START/FINISH address window (loading begins at START), and
// @address directives produced by a sparse image.
module tb;
  reg [7:0] src    [0:7];
  reg [7:0] dst    [0:7];
  reg [7:0] win    [0:7];
  reg [7:0] back   [0:7];
  reg [3:0] bsrc   [0:3];
  reg [3:0] bdst   [0:3];
  reg [7:0] sparse [0:7];
  integer i;
  reg ok;

  initial begin
    ok = 1'b1;
    for (i = 0; i < 8; i = i + 1) src[i] = i * 17;
    $writememh("hdlsim_mem.txt", src);
    $readmemh("hdlsim_mem.txt", dst);
    for (i = 0; i < 8; i = i + 1)
      if (dst[i] !== src[i]) ok = 1'b0;

    // Window: loading begins at START=4 and stops after FINISH=7, so the
    // first four file words land at win[4..7] and win[0..3] stay untouched.
    $readmemh("hdlsim_mem.txt", win, 4, 7);
    for (i = 0; i < 4; i = i + 1)
      if (win[i + 4] !== src[i]) ok = 1'b0;

    // Binary round trip.
    for (i = 0; i < 4; i = i + 1) bsrc[i] = i + 1;
    $writememb("hdlsim_mem.bin", bsrc);
    $readmemb("hdlsim_mem.bin", bdst);
    for (i = 0; i < 4; i = i + 1)
      if (bdst[i] !== bsrc[i]) ok = 1'b0;

    // Sparse image: the read back into pre-filled 8'hff memory restores the
    // x holes too.
    for (i = 0; i < 8; i = i + 1) back[i] = 8'hff;
    sparse[0] = 8'ha5;
    sparse[5] = 8'h3c;
    sparse[6] = 8'h7e;
    $writememh("hdlsim_sparse.txt", sparse);
    $readmemh("hdlsim_sparse.txt", back);
    for (i = 0; i < 8; i = i + 1)
      if (back[i] !== sparse[i]) ok = 1'b0;

    // Writing with START=5 emits a leading @5 directive; reading it back
    // lands the words at 5..7 and leaves 0..4 untouched.
    for (i = 0; i < 8; i = i + 1) back[i] = 8'hff;
    $writememh("hdlsim_tail.txt", sparse, 5, 7);
    $readmemh("hdlsim_tail.txt", back);
    for (i = 0; i < 5; i = i + 1)
      if (back[i] !== 8'hff) ok = 1'b0;
    for (i = 5; i < 8; i = i + 1)
      if (back[i] !== sparse[i]) ok = 1'b0;

    if (ok) begin
      $display("PASS");
      $finish;
    end
    $display("FAIL: memload dst[0]=%h win[4]=%h bdst[3]=%b back[5]=%h",
             dst[0], win[4], bdst[3], back[5]);
    $fatal(1);
  end
endmodule
