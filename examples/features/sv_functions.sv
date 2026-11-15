// User functions: ANSI args with return, named-return assignment, recursion.
module tb;
  function automatic int double(input int x); return 2 * x; endfunction
  function automatic int fact(input int n);
    if (n <= 1) return 1;
    return n * fact(n - 1);
  endfunction
  function int named_ret(input int x);
    named_ret = x + 1;
  endfunction
  initial begin
    if (double(21) == 42 && fact(5) == 120 && named_ret(9) == 10) $display("PASS");
    else $display("FAIL");
    $finish;
  end
endmodule
