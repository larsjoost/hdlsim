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

/// `hdlsim run file.vhd` without `--top`: a VHDL file lowers to an entity
/// module plus an `entity/arch` variant per architecture, so "multiple
/// modules" must not fire — the single entity is picked (regression test for
/// the issue #106 reproducer `hdlsim run unit_tests_behavior.vhd`).
#[test]
fn run_without_top_picks_single_entity_over_arch_variants() {
    let f = tmp(
        "unit_tests_behavior.vhd",
        "library std;\nuse std.env.all;\nentity unit_tests is end entity;\narchitecture behavior of unit_tests is\nbegin\n  process\n  begin\n    report \"PASS: unit_tests\";\n    std.env.finish;\n    wait;\n  end process;\nend architecture;\n",
    );
    let out = hdlsim()
        .args(["run", f.to_str().unwrap()])
        .output()
        .expect("runs");
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success(),
        "expected exit 0, got {:?}\n{}",
        out.status,
        all
    );
    assert!(all.contains("PASS: unit_tests"), "{}", all);
    assert!(!all.contains("multiple modules"), "{}", all);
    let _ = std::fs::remove_file(&f);

    // Genuinely ambiguous (two entities) still asks for --top, listing only
    // the entity names, not the entity/arch variants.
    let f2 = tmp(
        "two_ent.vhd",
        "entity a is end entity;\narchitecture r of a is begin end architecture;\nentity b is end entity;\narchitecture r of b is begin end architecture;\n",
    );
    let out = hdlsim()
        .args(["run", f2.to_str().unwrap()])
        .output()
        .expect("runs");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(stderr.contains("multiple modules (a, b)"), "{}", stderr);
    assert!(!stderr.contains("a/r"), "{}", stderr);
    let _ = std::fs::remove_file(&f2);
}

/// Issue #113: `hdlsim run unit_tests_behavior.vhd --max-time 10ms --progress`
/// exited 2 with "elaboration error: ... port 'b' not connected" when a port
/// map omits ports or uses `open` — legal VHDL (in-ports with defaults, open
/// out-ports) that ModelSim accepts. Unconnected ports must elaborate to a
/// private signal (initialised to the port default), not a hard error.
#[test]
fn run_with_omitted_and_open_ports() {
    let dir = std::env::temp_dir().join(format!("hdlsim_uncon_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("dut.vhd"),
        "library ieee;\nuse ieee.std_logic_1164.all;\nentity dut is\n  port ( a : in std_logic;\n         b : in std_logic := '1';\n         y : out std_logic;\n         z : out std_logic_vector(3 downto 0) );\nend entity;\narchitecture rtl of dut is\nbegin\n  y <= a and b;\n  z <= (others => a);\nend architecture;\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("unit_tests_behavior.vhd"),
        "library ieee;\nuse ieee.std_logic_1164.all;\nuse std.env.all;\nentity unit_tests is end entity;\narchitecture behavior of unit_tests is\n  signal a : std_logic := '0';\n  signal y : std_logic;\nbegin\n  -- b omitted (has default '1'), z left open.\n  uut : entity work.dut port map ( a => a, y => y );\n  -- explicit `open` on in/out ports must not be an elaboration error either.\n  uut2 : entity work.dut port map ( a => a, b => open, y => open, z => open );\n  process\n  begin\n    a <= '1';\n    wait for 10 ns;\n    -- only passes if the omitted port b got its declared default '1'\n    assert y = '1' report \"defaulted port failed, y=\" & std_logic'image(y) severity error;\n    report \"PASS: unit_tests omitted/open ports\";\n    std.env.finish;\n    wait;\n  end process;\nend architecture;\n",
    )
    .unwrap();
    let out = hdlsim()
        .current_dir(&dir)
        .args([
            "run",
            "unit_tests_behavior.vhd",
            "--max-time",
            "10ms",
            "--progress",
        ])
        .output()
        .expect("runs");
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success(),
        "expected exit 0, got {:?}\n{}",
        out.status,
        all
    );
    assert!(
        all.contains("PASS: unit_tests omitted/open ports"),
        "{}",
        all
    );
    assert!(!all.contains("not connected"), "{}", all);
    let _ = std::fs::remove_dir_all(&dir);
}
