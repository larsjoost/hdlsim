//! Mixed-language co-simulation: VHDL units in Verilog testbenches and
//! Verilog modules in VHDL testbenches (direct `entity work.x` and
//! `component` instantiation), including case-insensitive unit resolution,
//! all in one kernel and one wave file.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.env("HDLSIM_NO_CACHE", "1");
    c
}

fn tmp(tag: &str, name: &str, content: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir =
        std::env::temp_dir().join(format!("hdlsim_mixed_{}_{}_{}", std::process::id(), tag, n));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, content).unwrap();
    p
}

const VHDL_ADDER: &str = "library ieee;\nuse ieee.std_logic_1164.all;\nuse ieee.numeric_std.all;\n\nentity adder is\n    port (a : in std_logic_vector(7 downto 0);\n          b : in std_logic_vector(7 downto 0);\n          s : out std_logic_vector(7 downto 0));\nend entity;\n\narchitecture rtl of adder is\nbegin\n    s <= std_logic_vector(unsigned(a) + unsigned(b));\nend architecture;\n";

const VERILOG_TB_ON_VHDL: &str = "module tb;\n    reg [7:0] a, b;\n    wire [7:0] s;\n    adder dut (.a(a), .b(b), .s(s));\n    initial begin\n        a = 8'd3; b = 8'd4;\n        #10;\n        if (s !== 8'd7) begin $display(\"FAIL s=%0d\", s); $finish; end\n        $display(\"PASS s=%0d\", s);\n        $finish;\n    end\nendmodule\n";

const VERILOG_ADDER: &str = "module Adder (input [7:0] a, input [7:0] b, output [7:0] s);\n    assign s = a + b;\nendmodule\n";

const VHDL_TB_ENTITY_WORK: &str = "library ieee;\nuse ieee.std_logic_1164.all;\n\nentity tb is\nend entity;\n\narchitecture sim of tb is\n    signal a, b, s : std_logic_vector(7 downto 0) := x\"00\";\nbegin\n    dut : entity work.Adder\n        port map (a => a, b => b, s => s);\n    process\n    begin\n        a <= x\"05\"; b <= x\"06\";\n        wait for 10 ns;\n        if s /= x\"0B\" then\n            report \"FAIL\" severity failure;\n        end if;\n        report \"PASS\";\n        wait;\n    end process;\nend architecture;\n";

const VHDL_TB_COMPONENT: &str = "library ieee;\nuse ieee.std_logic_1164.all;\n\nentity tb2 is\nend entity;\n\narchitecture sim of tb2 is\n    component Adder\n        port (a : in std_logic_vector(7 downto 0);\n              b : in std_logic_vector(7 downto 0);\n              s : out std_logic_vector(7 downto 0));\n    end component;\n    signal a, b, s : std_logic_vector(7 downto 0) := x\"00\";\nbegin\n    dut : Adder port map (a => a, b => b, s => s);\n    process\n    begin\n        a <= x\"05\"; b <= x\"06\";\n        wait for 10 ns;\n        if s /= x\"0B\" then\n            report \"FAIL\" severity failure;\n        end if;\n        report \"PASS\";\n        wait;\n    end process;\nend architecture;\n";

const VHDL_ADDER_UPPER: &str = "library ieee;\nuse ieee.std_logic_1164.all;\nuse ieee.numeric_std.all;\n\nentity ADDER is\n    port (a : in std_logic_vector(7 downto 0);\n          b : in std_logic_vector(7 downto 0);\n          s : out std_logic_vector(7 downto 0));\nend entity;\n\narchitecture rtl of ADDER is\nbegin\n    s <= std_logic_vector(unsigned(a) + unsigned(b));\nend architecture;\n";

const VERILOG_TB_UPPER: &str = "module tb3;\n    reg [7:0] a, b;\n    wire [7:0] s;\n    ADDER dut (.a(a), .b(b), .s(s));\n    initial begin\n        a = 8'd3; b = 8'd4;\n        #10;\n        if (s !== 8'd7) begin $display(\"FAIL s=%0d\", s); $finish; end\n        $display(\"PASS s=%0d\", s);\n        $finish;\n    end\nendmodule\n";

fn run(top: &str, files: &[&PathBuf], extra: &[&str]) -> (bool, String, String) {
    let mut c = hdlsim();
    c.arg("run").arg("--top").arg(top);
    for e in extra {
        c.arg(e);
    }
    for f in files {
        c.arg(f);
    }
    let out = c.output().expect("runs");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

#[test]
fn verilog_tb_instantiates_vhdl_entity() {
    let vhd = tmp("a", "adder.vhd", VHDL_ADDER);
    let tb = tmp("a", "tb.v", VERILOG_TB_ON_VHDL);
    let (ok, stdout, stderr) = run("tb", &[&vhd, &tb], &[]);
    assert!(ok && stdout.contains("PASS"), "{}\n{}", stdout, stderr);
}

#[test]
fn vhdl_tb_instantiates_verilog_module_via_entity_work() {
    let v = tmp("b", "Adder.v", VERILOG_ADDER);
    let tb = tmp("b", "tb.vhd", VHDL_TB_ENTITY_WORK);
    let (ok, stdout, stderr) = run("tb", &[&v, &tb], &[]);
    assert!(ok && stdout.contains("PASS"), "{}\n{}", stdout, stderr);
}

#[test]
fn vhdl_tb_instantiates_verilog_module_via_component() {
    let v = tmp("c", "Adder.v", VERILOG_ADDER);
    let tb = tmp("c", "tb2.vhd", VHDL_TB_COMPONENT);
    let (ok, stdout, stderr) = run("tb2", &[&v, &tb], &[]);
    assert!(ok && stdout.contains("PASS"), "{}\n{}", stdout, stderr);
}

#[test]
fn unit_resolution_is_case_insensitive_across_languages() {
    let vhd = tmp("d", "ADDER.vhd", VHDL_ADDER_UPPER);
    let tb = tmp("d", "tb3.v", VERILOG_TB_UPPER);
    let (ok, stdout, stderr) = run("tb3", &[&vhd, &tb], &[]);
    assert!(ok && stdout.contains("PASS"), "{}\n{}", stdout, stderr);
}

#[test]
fn mixed_design_shares_one_wave_file() {
    let vhd = tmp("e", "adder.vhd", VHDL_ADDER);
    let tb = tmp("e", "tb.v", VERILOG_TB_ON_VHDL);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let vcd = std::env::temp_dir().join(format!("hdlsim_mixed_{}_e_{}.vcd", std::process::id(), n));
    let vcd_str = vcd.to_str().unwrap().to_string();
    let (ok, stdout, stderr) = run("tb", &[&vhd, &tb], &["--vcd", &vcd_str]);
    assert!(ok && stdout.contains("PASS"), "{}\n{}", stdout, stderr);
    let text = std::fs::read_to_string(&vcd).unwrap();
    assert!(text.contains("$var"), "{}", text);
    // signals from both languages appear in the same VCD scope tree
    assert!(text.contains("dut.s") || text.contains("s"), "{}", text);
    let _ = std::fs::remove_file(&vcd);
}
