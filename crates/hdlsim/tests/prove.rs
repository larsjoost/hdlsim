//! End-to-end tests for `hdlsim prove` (bounded model checking with the own
//! CDCL solver): safe designs exit 0, violated properties exit 1 with a
//! counterexample trace, unsupported constructs are clean errors (exit 2).

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.env("HDLSIM_NO_CACHE", "1");
    c
}

fn tmp(name: &str, content: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("hdlsim_prove_{}_{}", std::process::id(), name));
    std::fs::write(&p, content).unwrap();
    p
}

fn prove(f: &PathBuf, top: &str, bmc: usize) -> std::process::Output {
    hdlsim()
        .args([
            "prove",
            "--top",
            top,
            "--bmc",
            &bmc.to_string(),
            f.to_str().unwrap(),
        ])
        .output()
        .expect("runs")
}

#[test]
fn safe_property_exits_0() {
    let f = tmp(
        "safe.v",
        "module dut(input wire clk, input wire [7:0] d, output reg [7:0] q);\n\
         always @(posedge clk) q <= d;\n\
         assert property (@(posedge clk) q < 256);\n\
         endmodule\n",
    );
    let out = prove(&f, "dut", 8);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("SAFE"), "{}", stdout);
    let _ = std::fs::remove_file(&f);
}

#[test]
fn violated_property_exits_1_with_cex() {
    // Accumulator can reach 15: q <= q + d with free d violates q != 15.
    let f = tmp(
        "bad.v",
        "module dut(input wire clk, input wire [3:0] d, output reg [3:0] q);\n\
         initial q = 0;\n\
         always @(posedge clk) q <= q + d;\n\
         assert property (@(posedge clk) q != 4'd15);\n\
         endmodule\n",
    );
    let out = prove(&f, "dut", 20);
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("VIOLATION"), "{}", stdout);
    assert!(stdout.contains("counterexample"), "{}", stdout);
    // The trace must contain q=15 at the violating step.
    assert!(stdout.contains("q=15"), "{}", stdout);
    let _ = std::fs::remove_file(&f);
}

#[test]
fn bounded_safe_when_violation_is_beyond_bmc_depth() {
    // 4-bit counter from 0 reaches 15 exactly at step 15; with --bmc 10 the
    // property cnt != 15 is safe within the bound.
    let f = tmp(
        "cnt.v",
        "module dut(input wire clk, output reg [3:0] cnt);\n\
         initial cnt = 0;\n\
         always @(posedge clk) cnt <= cnt + 1;\n\
         assert property (@(posedge clk) cnt != 4'd15);\n\
         endmodule\n",
    );
    let out = prove(&f, "dut", 10);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("SAFE"));
    // With enough depth the violation is found at exactly step 15.
    let out = prove(&f, "dut", 20);
    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("fails at step 15"), "{}", stdout);
    let _ = std::fs::remove_file(&f);
}

#[test]
fn next_cycle_property_violation() {
    // b <= a, but the property claims b == a in the same cycle (|->) → fails.
    let f = tmp(
        "ovl.v",
        "module dut(input wire clk, input wire a, output reg b);\n\
         always @(posedge clk) b <= a;\n\
         assert property (@(posedge clk) a |-> b);\n\
         endmodule\n",
    );
    let out = prove(&f, "dut", 4);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stdout).contains("VIOLATION"));
    let _ = std::fs::remove_file(&f);
}

#[test]
fn next_cycle_property_safe() {
    // b <= a; a |=> b holds one cycle later.
    let f = tmp(
        "nxt.v",
        "module dut(input wire clk, input wire a, output reg b);\n\
         always @(posedge clk) b <= a;\n\
         assert property (@(posedge clk) a |=> b);\n\
         endmodule\n",
    );
    let out = prove(&f, "dut", 4);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let _ = std::fs::remove_file(&f);
}

#[test]
fn unsupported_memory_is_clean_error() {
    let f = tmp(
        "mem.v",
        "module dut(input wire clk, input wire [3:0] d, output reg [7:0] q);\n\
         reg [7:0] mem [0:15];\n\
         always @(posedge clk) begin mem[0] <= d; q <= mem[0]; end\n\
         assert property (@(posedge clk) q == 0);\n\
         endmodule\n",
    );
    let out = prove(&f, "dut", 4);
    assert_eq!(
        out.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("memories"));
    let _ = std::fs::remove_file(&f);
}

#[test]
fn no_property_is_error() {
    let f = tmp(
        "noprop.v",
        "module dut(input wire clk, output reg q);\n\
         always @(posedge clk) q <= ~q;\n\
         endmodule\n",
    );
    let out = prove(&f, "dut", 4);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("assert property"));
    let _ = std::fs::remove_file(&f);
}

#[test]
fn prove_works_with_library_resolution() {
    // Child resolved from a -L library, top is the DUT in the file.
    let dir = std::env::temp_dir().join(format!("hdlsim_prove_lib_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("adder.v"),
        "module adder(input wire clk, input wire [3:0] a, input wire [3:0] b, output wire [4:0] s);\n\
         assign s = a + b;\n\
         assert property (@(posedge clk) s < 32);\n\
         endmodule\n",
    )
    .unwrap();
    let f = tmp(
        "top.v",
        "module top(input wire clk, input wire [3:0] a, input wire [3:0] b, output wire [4:0] s);\n\
         adder u (.clk(clk), .a(a), .b(b), .s(s));\n\
         assert property (@(posedge clk) s == a + b);\n\
         endmodule\n",
    );
    let out = hdlsim()
        .args([
            "prove",
            "--top",
            "top",
            "--bmc",
            "3",
            "-L",
            &format!("work={}", dir.display()),
            f.to_str().unwrap(),
        ])
        .output()
        .expect("runs");
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("2 propert"), "{}", stdout); // both asserts checked
    assert!(stdout.contains("SAFE"), "{}", stdout);
    let _ = std::fs::remove_file(&f);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn vhdl_assert_is_provable() {
    // VHDL DUT with a clocked process and an assert; property holds → safe.
    let f = tmp(
        "dut.vhd",
        "library ieee;\n\
         use ieee.std_logic_1164.all;\n\
         use ieee.numeric_std.all;\n\
         entity dut is\n\
           port(clk : in std_logic; d : in unsigned(7 downto 0); q : out unsigned(7 downto 0));\n\
         end entity;\n\
         architecture rtl of dut is\n\
           signal r : unsigned(7 downto 0) := x\"00\";\n\
         begin\n\
           q <= r;\n\
           process(clk) begin\n\
             if rising_edge(clk) then\n\
               r <= d;\n\
               assert r < 256 report \"overflow\" severity failure;\n\
             end if;\n\
           end process;\n\
         end architecture;\n",
    );
    let out = prove(&f, "dut", 4);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{}{}", stdout, stderr);
    assert!(stdout.contains("SAFE"), "{}", stdout);
    let _ = std::fs::remove_file(&f);
}
