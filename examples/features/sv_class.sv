// Classes: fields, constructor, methods, task, randomize with constraints.
module tb;
  class C;
    rand int x;
    rand int y;
    int tag;
    function new(int t); tag = t; x = 0; y = 0; endfunction
    function int sum(); return x + y; endfunction
    task setx(int v); x = v; endtask
    constraint c { x > 10; x < 20; x + y == 30; }
  endclass
  C c;
  initial begin
    c = new C(7);
    c.setx(15);
    if (c.tag != 7 || c.x != 15 || c.sum() != 15) $display("FAIL fields");
    else if (c.randomize() == 1 && c.x > 10 && c.x < 20 && c.x + c.y == 30 && c.tag == 7)
      $display("PASS");
    else $display("FAIL rand %0d %0d", c.x, c.y);
    $finish;
  end
endmodule
