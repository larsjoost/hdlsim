// Procedural continuous assignment (LRM 9.2.2): `assign lhs = rhs;` inside a
// process drives lhs continuously until `deassign lhs;`, which stops the
// driving but leaves lhs holding its last value; a later procedural
// assignment works on it again. force/release are the net-level counterparts.
module tb;
  reg [3:0] q, d;
  reg [3:0] q2;
  reg       en;
  initial begin
    q  = 4'h0;
    d  = 4'h3;
    q2 = 4'h0;
    en = 1'b1;

    // Continuous: q follows every change of d.
    #5 assign q = d;
    #5;                       // t=10: q == 3
    d = 4'h7;
    #5;                       // t=15: q == 7

    // deassign stops the driving; q keeps 7 while d moves on.
    deassign q;
    d = 4'hf;
    #5;                       // t=20: q == 7

    // A plain assignment works again on the deassigned variable.
    q = 4'h1;
    #5;                       // t=25: q == 1

    // Combinational gate driven by a procedural continuous assignment.
    assign q2 = d & {4{en}};
    #5;                       // t=30: q2 == 0xf & 1 == 15
    en = 1'b0;
    #5;                       // t=35: q2 == 0
    deassign q2;
    en = 1'b1;
    #5;                       // t=40: q2 still 0 (drive removed)

    if (q === 4'h1 && q2 === 4'h0) begin
      $display("PASS");
      $finish;
    end
    $display("FAIL q=%h (expect 1) q2=%h (expect 0)", q, q2);
    $fatal(1);
  end
endmodule
