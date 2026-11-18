//! SystemVerilog feature tests: packages + import + `pkg::` refs, typedefs
//! (plain/enum/packed struct), assignment patterns `'{...}`, struct field
//! selects, dynamic/queue/assoc arrays with `new[]`/`push_back`/`size()`/
//! `foreach`, user functions with `return`, and the fixed postfix parsing of
//! `a[i]`, `a[msb:lsb]`, and `f(x)`.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.env("HDLSIM_NO_CACHE", "1");
    c
}

fn run_src(name: &str, src: &str) -> String {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let p = std::env::temp_dir().join(format!(
        "hdlsim_sv_{}_{}_{}.sv",
        std::process::id(),
        n,
        name
    ));
    std::fs::write(&p, src).unwrap();
    let out = hdlsim()
        .args(["run", "--top", "tb"])
        .arg(&p)
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_file(&p);
    text
}

fn assert_pass(name: &str, src: &str) {
    let out = run_src(name, src);
    assert!(
        out.lines().any(|l| l.trim() == "PASS"),
        "{}: expected a PASS line, got:\n{}",
        name,
        out
    );
    assert!(
        !out.contains("error"),
        "{}: unexpected compile error:\n{}",
        name,
        out
    );
}

#[test]
fn package_import_and_scoped_ref() {
    assert_pass(
        "package",
        r#"
package p;
  parameter int W = 8;
  typedef logic [W-1:0] word_t;
  function automatic int double(input int x); return 2*x; endfunction
endpackage
module tb;
  import p::*;
  word_t w;
  initial begin
    w = p::double(21);
    if (w == 42 && W == 8) $display("PASS"); else $display("FAIL %0d", w);
    $finish;
  end
endmodule
"#,
    );
}

#[test]
fn enum_typedef_and_labels() {
    assert_pass(
        "enum",
        r#"
module tb;
  typedef enum logic [1:0] {IDLE, RUN, DONE} state_t;
  state_t s;
  initial begin
    s = RUN;
    if (s == RUN && s != IDLE && DONE == 2) $display("PASS"); else $display("FAIL");
    $finish;
  end
endmodule
"#,
    );
}

#[test]
fn packed_struct_fields_and_pattern() {
    assert_pass(
        "struct",
        r#"
module tb;
  typedef struct packed { logic [7:0] a; logic [3:0] b; } pair_t;
  pair_t p;
  initial begin
    p = '{a: 8'h3C, b: 4'h5};
    if (p.a == 8'h3C && p.b == 4'h5) $display("PASS"); else $display("FAIL");
    $finish;
  end
endmodule
"#,
    );
}

#[test]
fn dynamic_queue_assoc_arrays_and_foreach() {
    assert_pass(
        "arrays",
        r#"
module tb;
  int da[];
  int q[$];
  int aa[string];
  initial begin
    da = new[3];
    da[0] = 1; da[1] = 2; da[2] = 3;
    q.push_back(10); q.push_back(20);
    aa["k"] = 99;
    foreach (da[i]) da[i] = da[i] * 10;
    if (da[2] == 30 && q[1] == 20 && aa["k"] == 99 && q.size() == 2) $display("PASS");
    else $display("FAIL");
    $finish;
  end
endmodule
"#,
    );
}

#[test]
fn user_function_with_return() {
    assert_pass(
        "function_return",
        r#"
module tb;
  function int double(input int x); return 2*x; endfunction
  reg [31:0] r;
  initial begin
    r = double(21);
    if (r == 42) $display("PASS"); else $display("FAIL %0d", r);
    $finish;
  end
endmodule
"#,
    );
}

#[test]
fn memory_index_and_part_select() {
    assert_pass(
        "mem_select",
        r#"
module tb;
  reg [7:0] m [0:3];
  reg [7:0] x;
  reg [3:0] hi;
  initial begin
    m[2] = 8'hAB;
    x = m[2];
    hi = x[7:4];
    if (x == 8'hAB && hi == 4'hA) $display("PASS"); else $display("FAIL");
    $finish;
  end
endmodule
"#,
    );
}

#[test]
fn always_ff_posedge() {
    assert_pass(
        "always_ff",
        r#"
module tb;
  logic clk = 0;
  logic [7:0] q = 0;
  always_ff @(posedge clk) q <= q + 1;
  always #5 clk = ~clk;
  initial begin
    #12 if (q == 1) $display("PASS"); else $display("FAIL %0d", q);
    $finish;
  end
endmodule
"#,
    );
}

#[test]
fn class_ctor_fields_and_methods() {
    assert_pass(
        "class_ctor",
        r#"
module tb;
  class C;
    int x;
    int y;
    function new(int a, int b); x = a; y = b; endfunction
    function int sum(); return x + y; endfunction
    task setx(int v); x = v; endtask
  endclass
  C c;
  initial begin
    c = new C(3, 4);
    if (c.sum() != 7) $display("FAIL sum %0d", c.sum());
    c.setx(10);
    if (c.x != 10 || c.sum() != 14) $display("FAIL %0d %0d", c.x, c.sum());
    else $display("PASS");
    $finish;
  end
endmodule
"#,
    );
}

#[test]
fn class_decl_init_new_and_field_read() {
    assert_pass(
        "class_decl_init",
        r#"
module tb;
  class C;
    int x;
    function new(int v); x = v; endfunction
    function int get(); return x; endfunction
  endclass
  C c = new(42);
  initial begin
    if (c.get() == 42 && c.x == 42) $display("PASS"); else $display("FAIL");
    $finish;
  end
endmodule
"#,
    );
}

#[test]
fn class_randomize_with_constraints() {
    assert_pass(
        "class_randomize",
        r#"
module tb;
  class C;
    rand int x;
    rand int y;
    constraint c { x > 10; x < 20; x + y == 30; }
  endclass
  C c;
  initial begin
    c = new C();
    if (c.randomize() != 1) $display("FAIL randomize returned 0");
    if (c.x > 10 && c.x < 20 && c.x + c.y == 30) $display("PASS");
    else $display("FAIL %0d %0d", c.x, c.y);
    $finish;
  end
endmodule
"#,
    );
}

#[test]
fn class_randomize_statement_form() {
    assert_pass(
        "class_randomize_stmt",
        r#"
module tb;
  class C;
    rand int x;
    constraint c { x >= 5; x <= 7; }
  endclass
  C c = new();
  initial begin
    c.randomize();
    if (c.x >= 5 && c.x <= 7) $display("PASS"); else $display("FAIL %0d", c.x);
    $finish;
  end
endmodule
"#,
    );
}
