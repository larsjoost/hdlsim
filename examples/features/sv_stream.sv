// SV streaming operators (LRM 11.4.14): `{>>{e}}` streams the operand
// unchanged; `{<<W{e}}` re-streams it in W-wide chunks, LSB chunk first —
// with W=1 the bit order reverses. Chunk widths that do not divide the
// operand width leave the short top chunk's missing bits X.
module tb;
  reg [3:0] a = 4'b1010;
  reg [7:0] w = 8'b1100_0011;
  reg [3:0] id, rev, rev2, rev4;
  reg [7:0] rev3;
  integer fails = 0;

  task check4(input [3:0] got, input [3:0] want, input [127:0] label);
    if (got !== want) begin
      $display("FAIL %0s: %b != %b", label, got, want);
      fails = fails + 1;
    end
  endtask

  initial begin
    id   = {>>{a}};   // >>: identity
    rev  = {<<{a}};   // << bit-by-bit: 1010 -> 0101
    rev4 = {<<4{a}};  // 4-bit chunks of a 4-bit value: identity
    rev2 = {<<2{a}};  // 2-bit chunks: 10|10 -> 10|10
    rev3 = {<<3{w}};  // 3-bit chunks of 8 bits: short top chunk -> X
    check4(id,   4'b1010, "identity");
    check4(rev,  4'b0101, "bit-reverse");
    check4(rev4, 4'b1010, "4-bit chunks");
    check4(rev2, 4'b1010, "2-bit chunks");
    // 11000011 sliced LSB-first in 3s: 011|000|11(short). Reversed, the
    // short chunk comes first; its missing third bit is X.
    if (rev3 !== 8'b11000x11) begin
      $display("FAIL 3-bit chunks: %b", rev3);
      fails = fails + 1;
    end
    if (fails == 0) $display("PASS"); else $display("FAIL %0d check(s)", fails);
    $finish;
  end
endmodule
