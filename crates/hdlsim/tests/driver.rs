//! End-to-end tests for driver tracing: `hdlsim debug --driver SIG` reports
//! which process (or continuous assign) last drove a signal, plus a
//! regression test that two Verilog modules in one file compile (the
//! per-module name map used to leak between modules).

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.env("HDLSIM_NO_CACHE", "1");
    c
}

fn examples() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples"))
}

fn tmp(name: &str, content: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("hdlsim_driver_{}_{}", std::process::id(), name));
    std::fs::write(&p, content).unwrap();
    p
}

#[test]
fn driver_of_process_driven_signal() {
    let tb = examples().join("counter/tb_counter.v");
    let out = hdlsim()
        .args([
            "debug",
            "--top",
            "tb_counter",
            "--driver",
            "q",
            "-L",
            &format!("work={}", examples().join("counter").display()),
            tb.to_str().unwrap(),
        ])
        .output()
        .expect("runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("driver of q: always @(posedge clk)"),
        "{}",
        stdout
    );
}

#[test]
fn driver_of_continuous_assign_and_initial() {
    let f = tmp(
        "assign_drv.v",
        "module m (input wire [3:0] a, output wire [3:0] y);\n    assign y = a + 4'd1;\nendmodule\nmodule tb_m;\n    reg [3:0] a;\n    wire [3:0] y;\n    m dut (.a(a), .y(y));\n    initial begin\n        a = 0;\n        #10 a = 4'd5;\n        #10 $finish;\n    end\nendmodule\n",
    );
    let out = hdlsim()
        .args([
            "debug",
            "--top",
            "tb_m",
            "--driver",
            "y",
            "--driver",
            "a",
            f.to_str().unwrap(),
        ])
        .output()
        .expect("runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("driver of y: continuous assign"),
        "{}",
        stdout
    );
    assert!(stdout.contains("driver of a: initial"), "{}", stdout);
    let _ = std::fs::remove_file(&f);
}

#[test]
fn driver_of_vhdl_signal() {
    let dir = examples().join("vhdl-counter");
    let tb = dir.join("tb_counter.vhd");
    let out = hdlsim()
        .args([
            "debug",
            "--top",
            "tb_counter",
            "--driver",
            "q",
            "-L",
            &format!("work={}", dir.display()),
            tb.to_str().unwrap(),
        ])
        .output()
        .expect("runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // q of the DUT is driven by the concurrent assignment q <= std_logic_vector(q_s).
    assert!(
        stdout.contains("driver of q: continuous assign"),
        "{}",
        stdout
    );
}

#[test]
fn two_modules_in_one_file_compile() {
    let f = tmp(
        "twomod.v",
        "module a (input wire x); endmodule\nmodule b;\n    reg x;\n    initial begin x = 1'b0; #10 $finish; end\nendmodule\n",
    );
    let out = hdlsim()
        .args(["check", f.to_str().unwrap()])
        .output()
        .expect("runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_file(&f);
}
