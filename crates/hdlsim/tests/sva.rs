//! End-to-end tests for SVA-lite: concurrent `assert property` with `|->`
//! (overlapping, pre-NBA sample) and `|=>` (next edge), immediate `assert`
//! statements, and assume/cover stubs. A failed assertion must exit 1.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

fn tmp(name: &str, content: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("hdlsim_sva_{}_{}", std::process::id(), name));
    std::fs::write(&p, content).unwrap();
    p
}

fn run(f: &PathBuf) -> std::process::Output {
    hdlsim()
        .args(["run", "--top", "tb", f.to_str().unwrap()])
        .output()
        .expect("runs")
}

#[test]
fn concurrent_assert_next_cycle_passes() {
    let f = tmp(
        "pass.v",
        "module m (input wire clk, input wire a, output reg b);\n    always @(posedge clk) b <= a;\n    assert property (@(posedge clk) a |=> b);\nendmodule\nmodule tb;\n    reg clk = 0; reg a = 0; wire b;\n    m dut (.clk(clk), .a(a), .b(b));\n    always #5 clk = ~clk;\n    initial begin\n        #12 a = 1;\n        #30 a = 0;\n        #40 $display(\"DONE\");\n        #1 $finish;\n    end\nendmodule\n",
    );
    let out = run(&f);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("DONE"), "{}", stdout);
    assert!(!stdout.contains("assertion failed"), "{}", stdout);
    let _ = std::fs::remove_file(&f);
}

#[test]
fn concurrent_assert_overlap_fails_and_exits_1() {
    // b is registered, so at the edge where a=1, b still holds the old value:
    // `a |-> b` (overlapping, pre-NBA) must fail. rc=1 via TestFailed.
    let f = tmp(
        "fail.v",
        "module m (input wire clk, input wire a, output reg b);\n    always @(posedge clk) b <= ~a;\n    assert property (@(posedge clk) a |-> b);\nendmodule\nmodule tb;\n    reg clk = 0; reg a = 0; wire b;\n    m dut (.clk(clk), .a(a), .b(b));\n    always #5 clk = ~clk;\n    initial begin\n        #12 a = 1;\n        #20 $finish;\n    end\nendmodule\n",
    );
    let out = run(&f);
    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("assertion failed"), "{}", stdout);
    assert!(stdout.contains("FAILED"), "{}", stdout);
    let _ = std::fs::remove_file(&f);
}

#[test]
fn immediate_assert_statement() {
    let f = tmp(
        "imm.v",
        "module tb;\n    reg [7:0] cnt = 0;\n    initial begin\n        cnt = 5;\n        assert (cnt == 8'd5);\n        assert (cnt < 100) else $display(\"overflow\");\n        #10 assert (cnt == 8'd5) else $error(\"changed\");\n        #10 $finish;\n    end\nendmodule\n",
    );
    let out = run(&f);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("assertion failed"), "{}", stdout);
    assert!(!stdout.contains("overflow"), "{}", stdout);

    // Failing immediate assert -> exit 1.
    let f2 = tmp(
        "imm_fail.v",
        "module tb;\n    reg [7:0] cnt = 0;\n    initial begin\n        cnt = 5;\n        assert (cnt == 8'd6);\n        #10 $finish;\n    end\nendmodule\n",
    );
    let out = run(&f2);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stdout).contains("assertion failed"));
    let _ = std::fs::remove_file(&f);
    let _ = std::fs::remove_file(&f2);
}

#[test]
fn assume_and_cover_are_parsed_with_a_warning() {
    let f = tmp(
        "stub.v",
        "module m (input wire clk, input wire a);\n    assume property (@(posedge clk) a);\n    cover property (@(posedge clk) a);\nendmodule\nmodule tb;\n    reg clk = 0; reg a = 0;\n    m dut (.clk(clk), .a(a));\n    always #5 clk = ~clk;\n    initial begin #12 a = 1; #10 $finish; end\nendmodule\n",
    );
    let out = run(&f);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("W0401"), "{}", stderr);
    assert!(stderr.contains("not simulated"), "{}", stderr);
    let _ = std::fs::remove_file(&f);
}
