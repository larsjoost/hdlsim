//! Regression tests for VHDL-specific lowering/kernel behavior discovered
//! while building the RISC-V example: signed comparisons, memory arrays with
//! constrained element types, part-selects, and case statements.

use hdl_ir::elab;
use hdl_sim::{Observer, SimEvent, Simulator};
use hdlsim::compile::compile_paths;
use std::sync::{Arc, Mutex};

struct Messages(Arc<Mutex<Vec<String>>>);
impl Observer for Messages {
    fn on_event(&mut self, ev: &SimEvent) {
        if let SimEvent::Message { text, .. } = ev {
            self.0.lock().unwrap().push(text.clone());
        }
    }
}

/// Run a VHDL snippet (entity `dut` + testbench `tb`) and return the transcript.
fn run_snippet(name: &str, dut: &str, tb: &str) -> String {
    let dir = std::env::temp_dir().join(format!("hdlsim_snippet_{}_{}", name, std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f1 = dir.join("dut.vhd");
    let f2 = dir.join("tb.vhd");
    std::fs::write(&f1, dut).unwrap();
    std::fs::write(&f2, tb).unwrap();
    let compiled = compile_paths(&[f1, f2]);
    assert!(!compiled.has_errors(), "{}", compiled.render_diags());
    let flat = elab::elaborate(&compiled.design, "tb").unwrap();
    let mut sim = Simulator::new(flat);
    let msgs = Arc::new(Mutex::new(Vec::new()));
    sim.add_observer(Box::new(Messages(msgs.clone())));
    sim.run(1_000_000_000);
    let log = msgs.lock().unwrap().join("");
    let _ = std::fs::remove_dir_all(&dir);
    log
}

/// Compile and run one VHDL testbench (top unit `tb`) and return its transcript.
fn run_tb(source: &str) -> String {
    let dir = std::env::temp_dir().join(format!("hdlsim_tb_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("tb.vhd");
    std::fs::write(&f, source).unwrap();
    let compiled = compile_paths(&[f]);
    assert!(!compiled.has_errors(), "{}", compiled.render_diags());
    let flat = elab::elaborate(&compiled.design, "tb").unwrap();
    let mut sim = Simulator::new(flat);
    let msgs = Arc::new(Mutex::new(Vec::new()));
    sim.add_observer(Box::new(Messages(msgs.clone())));
    sim.run(1_000_000_000);
    let log = msgs.lock().unwrap().join("");
    let _ = std::fs::remove_dir_all(&dir);
    log
}

const TB_HEAD: &str = r#"
library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;
entity tb is end entity;
architecture sim of tb is
    signal clk : std_logic := '0';
    signal rst : std_logic := '1';
    signal done : std_logic;
    signal result : unsigned(31 downto 0);
begin
    dut : entity work.dut port map (clk => clk, rst => rst, done => done, result => result);
    process begin
        clk <= not clk;
        wait for 5 ns;
    end process;
    process
    begin
        wait until rising_edge(clk);
        wait until rising_edge(clk);
        rst <= '0';
        wait until done = '1';
        wait for 1 ns;
"#;

const TB_TAIL: &str = r#"
        std.env.finish;
        wait;
    end process;
end architecture;
"#;

#[test]
fn signed_comparison_negative_less_than_positive() {
    const NAME: &str = "signed_cmp";
    // BLT-style: signed(-1) < signed(1) must be true.
    let dut = r#"
library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;
entity dut is
    port (clk : in std_logic; rst : in std_logic; done : out std_logic := '0'; result : out unsigned(31 downto 0) := x"00000000");
end entity;
architecture rtl of dut is
    signal a : signed(31 downto 0) := to_signed(-1, 32);
    signal b : signed(31 downto 0) := to_signed(1, 32);
begin
    process(clk)
    begin
        if rising_edge(clk) then
            if rst = '1' then
                result <= x"00000000";
            else
                if a < b then
                    result <= x"00000001";
                else
                    result <= x"00000000";
                end if;
                done <= '1';
            end if;
        end if;
    end process;
end architecture;
"#;
    let tb = format!(
        "{}{}{}",
        TB_HEAD,
        r#"        if to_integer(result) = 1 then
            report "PASS: signed -1 < 1";
        else
            report "FAIL: signed compare wrong" severity error;
        end if;"#,
        TB_TAIL
    );
    let log = run_snippet(NAME, dut, &tb);
    assert!(log.contains("PASS"), "transcript:\n{}", log);
}

#[test]
fn memory_array_with_constrained_element_and_dynamic_index() {
    const NAME: &str = "mem_array";
    // Mirrors the RISC-V register file / imem pattern.
    let dut = r#"
library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;
entity dut is
    port (clk : in std_logic; rst : in std_logic; done : out std_logic := '0'; result : out unsigned(31 downto 0) := x"00000000");
end entity;
architecture rtl of dut is
    type mem_t is array (0 to 7) of std_logic_vector(31 downto 0);
    signal mem : mem_t := (others => x"00000000");
    signal idx : integer := 0;
begin
    process(clk)
    begin
        if rising_edge(clk) then
            if rst = '1' then
                mem(3) <= x"0000002A";
                idx <= 0;
            elsif idx = 0 then
                result <= unsigned(mem(3));
                idx <= 1;
            else
                done <= '1';
            end if;
        end if;
    end process;
end architecture;
"#;
    let tb = format!(
        "{}{}{}",
        TB_HEAD,
        r#"        if to_integer(result) = 42 then
            report "PASS: mem read 42";
        else
            report "FAIL: mem read" severity error;
        end if;"#,
        TB_TAIL
    );
    let log = run_snippet(NAME, dut, &tb);
    assert!(log.contains("PASS"), "transcript:\n{}", log);
}

#[test]
fn part_select_bounds_are_not_confused_with_base() {
    const NAME: &str = "part_select";
    // `v(7 downto 0)` — msb/lsb must be 7/0, not the base signal.
    let dut = r#"
library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;
entity dut is
    port (clk : in std_logic; rst : in std_logic; done : out std_logic := '0'; result : out unsigned(31 downto 0) := x"00000000");
end entity;
architecture rtl of dut is
    signal v : unsigned(31 downto 0) := x"000000FF";
begin
    process(clk)
    begin
        if rising_edge(clk) then
            if rst = '1' then
                result <= x"00000000";
            else
                result <= x"00000000" & v(7 downto 0);
                done <= '1';
            end if;
        end if;
    end process;
end architecture;
"#;
    let tb = format!(
        "{}{}{}",
        TB_HEAD,
        r#"        if to_integer(result) = 255 then
            report "PASS: slice read 0xFF";
        else
            report "FAIL: slice read" severity error;
        end if;"#,
        TB_TAIL
    );
    let log = run_snippet(NAME, dut, &tb);
    assert!(log.contains("PASS"), "transcript:\n{}", log);
}

#[test]
fn case_statement_with_multi_stmt_arm() {
    const NAME: &str = "case_arm";
    let dut = r#"
library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;
entity dut is
    port (clk : in std_logic; rst : in std_logic; done : out std_logic := '0'; result : out unsigned(31 downto 0) := x"00000000");
end entity;
architecture rtl of dut is
    signal sel : std_logic_vector(1 downto 0) := "01";
begin
    process(clk)
        variable tmp : integer := 0;
    begin
        if rising_edge(clk) then
            if rst = '1' then
                result <= x"00000000";
            else
                case sel is
                    when "01" =>
                        tmp := 20;
                        tmp := tmp + 22;
                        result <= to_unsigned(tmp, 32);
                    when others =>
                        result <= x"00000000";
                end case;
                done <= '1';
            end if;
        end if;
    end process;
end architecture;
"#;
    let tb = format!(
        "{}{}{}",
        TB_HEAD,
        r#"        if to_integer(result) = 42 then
            report "PASS: case multi-stmt arm";
        else
            report "FAIL: case arm" severity error;
        end if;"#,
        TB_TAIL
    );
    let log = run_snippet(NAME, dut, &tb);
    assert!(log.contains("PASS"), "transcript:\n{}", log);
}

#[test]
fn integer_subtypes_keep_signed_two_complement_semantics() {
    // `natural`/`positive` are integer subtypes (LRM 1076, integer types):
    // they must keep integer's signed two's-complement semantics so that
    // `s < 0` sees a negative value. Lowering them as unsigned made the
    // UNIFORM (IEEE 1076.2 math_real) fixup `if s2 < 0 then s2 := s2 +
    // 2147483563` never fire — the seed wrapped negative and every later
    // draw was garbage.
    let tb = r#"
library ieee;
use ieee.std_logic_1164.all;
entity tb is end entity;
architecture sim of tb is
  -- IEEE 1076.2-1996 UNIFORM, inlined; the fixup on `s2 < 0` is the
  -- behavior under test.
  procedure lrm_uniform (variable s1, s2 : inout positive; variable x : out real) is
    variable k, z : integer;
  begin
    k := s1 / 53668;
    s1 := 40014 * (s1 - k * 53668) - k * 12211;
    if s1 < 0 then s1 := s1 + 2147483563; end if;
    k := s2 / 52774;
    s2 := 40692 * (s2 - k * 52774) - k * 3791;
    if s2 < 0 then s2 := s2 + 2147483563; end if;
    z := s1 - s2;
    if z < 1 then z := z + 2147483562; end if;
    x := real(z) * 4.656613e-10;
  end procedure;
begin
  main : process
    variable seed1 : positive := 1;
    variable seed2 : positive := 1;
    variable x : real;
  begin
    for step in 1 to 3 loop
      lrm_uniform(seed1, seed2, x);
      report "STEP " & integer'image(step)
        & " seeds=(" & integer'image(seed1) & "," & integer'image(seed2) & ")";
    end loop;
    wait;
  end process;
end architecture;
"#;
    let log = run_tb(tb);
    // Third LRM step: the s2 fixup must fire (2103410427 = -44073136 +
    // 2147483563), so the seeds stay inside the positive subtype.
    assert!(
        log.contains("STEP 3 seeds=(1346387765,2103410427)"),
        "transcript:\n{}",
        log
    );
}
