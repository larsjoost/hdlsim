//! End-to-end tests for `hdlsim analyze`: a module with a never-driven /
//! never-read wire must produce W0501/W0502 findings (exit 1, explained on
//! stderr), and a clean module must produce none (exit 0).

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

fn tmp(name: &str, content: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("hdlsim_analyze_{}_{}", std::process::id(), name));
    std::fs::write(&p, content).unwrap();
    p
}

#[test]
fn analyze_flags_dead_signals_with_explanations() {
    let f = tmp(
        "dead.v",
        "module m (\n    input  wire clk,\n    input  wire en,\n    output reg  [7:0] q\n);\n    wire [7:0] dead_wire;\n    always @(posedge clk) begin\n        if (en)\n            q <= q + 8'd1;\n    end\nendmodule\n",
    );
    let out = hdlsim()
        // --no-color: this test pins the finding text; colors are on by
        // default now (see tests/color.rs).
        .args(["analyze", "--no-color", f.to_str().unwrap()])
        .output()
        .expect("runs");
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("warning[W0501] m.dead_wire"), "{}", stderr);
    assert!(stderr.contains("warning[W0502] m.dead_wire"), "{}", stderr);
    assert!(stderr.contains("note[W0501]:"), "{}", stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("2 finding(s)"), "{}", stdout);
    let _ = std::fs::remove_file(&f);
}

#[test]
fn analyze_clean_design_passes() {
    let counter = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/counter/counter.v"
    );
    let out = hdlsim().args(["analyze", counter]).output().expect("runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("no findings"));
}

#[test]
fn analyze_vhdl_entity() {
    let f = tmp(
        "dead.vhd",
        "library ieee;\nuse ieee.std_logic_1164.all;\n\nentity m is\n    port (clk : in std_logic; q : out std_logic_vector(7 downto 0));\nend entity;\n\narchitecture rtl of m is\n    signal dead_s : std_logic_vector(7 downto 0);\nbegin\n    q <= dead_s;\nend architecture;\n",
    );
    let out = hdlsim()
        .args(["analyze", "--no-color", f.to_str().unwrap()])
        .output()
        .expect("runs");
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    // dead_s is read (q <= dead_s) but never driven.
    assert!(stderr.contains("warning[W0501] m.dead_s"), "{}", stderr);
    assert!(!stderr.contains("W0502] m.dead_s"), "{}", stderr);
    let _ = std::fs::remove_file(&f);
}
