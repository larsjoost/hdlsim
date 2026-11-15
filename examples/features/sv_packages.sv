// Packages: parameters, typedefs, functions; import p::*; and p::name refs.
package p;
  parameter int W = 8;
  typedef logic [W-1:0] word_t;
  function automatic int double(input int x); return 2 * x; endfunction
endpackage
module tb;
  import p::*;
  word_t w;
  initial begin
    w = p::double(21);
    if (w == 42) $display("PASS"); else $display("FAIL %0d", w);
    $finish;
  end
endmodule
