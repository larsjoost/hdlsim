//! VHDL-2008 feature tests: `process(all)`, matching case (`case?` with
//! don't-care choices), matching operators, `to_hstring`/`to_ostring`/
//! `to_bstring`, delayed and conditional concurrent signal assignments, and
//! the LRM rule that a process with a sensitivity list executes once at t=0.

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

/// Compile and run a single VHDL file with top entity `tb`; returns the
/// transcript of `report` messages.
fn run_vhdl(name: &str, src: &str) -> String {
    let dir = std::env::temp_dir().join(format!("hdlsim_vhdl2008_{}_{}", name, std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("tb.vhd");
    std::fs::write(&f, src).unwrap();
    let compiled = compile_paths(&[f]);
    assert!(!compiled.has_errors(), "{}", compiled.render_diags());
    let flat = elab::elaborate(&compiled.design, "tb").unwrap();
    let mut sim = Simulator::new(flat);
    let msgs = Arc::new(Mutex::new(Vec::new()));
    sim.add_observer(Box::new(Messages(msgs.clone())));
    sim.run(10_000_000_000);
    let log = msgs.lock().unwrap().join("\n");
    let _ = std::fs::remove_dir_all(&dir);
    log
}

fn assert_pass(name: &str, src: &str) {
    let log = run_vhdl(name, src);
    assert!(
        log.contains("PASS"),
        "{}: expected PASS in transcript, got:\n{}",
        name,
        log
    );
    assert!(
        !log.contains("FAIL"),
        "{}: unexpected FAIL in transcript:\n{}",
        name,
        log
    );
}

#[test]
fn process_all_sensitivity() {
    assert_pass(
        "process_all",
        r#"
entity tb is end;
architecture rtl of tb is
  signal a : bit := '0';
  signal seen : bit := '0';
begin
  process(all)
  begin
    if a = '1' then seen <= '1'; end if;
  end process;
  process
  begin
    wait for 2 ns;
    a <= '1';
    wait for 2 ns;
    if seen = '1' then report "PASS"; else report "FAIL"; end if;
    wait;
  end process;
end;
"#,
    );
}

#[test]
fn process_with_sens_list_runs_at_time_zero() {
    // LRM 11.3: a process executes once at the start of simulation.
    assert_pass(
        "sens_initial_run",
        r#"
entity tb is end;
architecture rtl of tb is
  signal s : bit := '1';
begin
  process(s)
  begin
    if s = '1' then report "PASS"; else report "FAIL"; end if;
  end process;
end;
"#,
    );
}

#[test]
fn matching_case_dont_care() {
    assert_pass(
        "caseq",
        r#"
entity tb is end;
architecture rtl of tb is
  signal s : bit_vector(3 downto 0) := "10-1";
begin
  process
  begin
    case? s is
      when "10-1" => report "PASS";
      when others => report "FAIL";
    end case;
    wait;
  end process;
end;
"#,
    );
}

#[test]
fn matching_case_no_match_falls_to_others() {
    assert_pass(
        "caseq_others",
        r#"
entity tb is end;
architecture rtl of tb is
  signal s : bit_vector(1 downto 0) := "11";
begin
  process
  begin
    case? s is
      when "0-" => report "FAIL";
      when "-0" => report "FAIL";
      when others => report "PASS";
    end case;
    wait;
  end process;
end;
"#,
    );
}

#[test]
fn matching_relational_operators() {
    assert_pass(
        "matching_ops",
        r#"
entity tb is end;
architecture rtl of tb is
  signal s : bit_vector(3 downto 0) := "1011";
begin
  process
  begin
    if s ?= "1---" and s ?/= "0---" and not (s ?= "0000") then
      report "PASS";
    else
      report "FAIL";
    end if;
    wait;
  end process;
end;
"#,
    );
}

#[test]
fn to_hstring_ostring_bstring() {
    assert_pass(
        "to_xstring",
        r#"
entity tb is end;
architecture rtl of tb is
  signal v : bit_vector(7 downto 0) := x"A5";
  signal w : bit_vector(5 downto 0) := "101011";
begin
  process
  begin
    if to_hstring(v) = "A5" and to_ostring(w) = "53" and to_bstring(w) = "101011" then
      report "PASS";
    else
      report "FAIL h=" & to_hstring(v) & " o=" & to_ostring(w) & " b=" & to_bstring(w);
    end if;
    wait;
  end process;
end;
"#,
    );
}

#[test]
fn single_char_string_literal_is_text() {
    // "h"/"u"/"w"/"l" spell words: a one-character string literal is the
    // STRING "h", not a 1-bit weak metavalue — concat and compare must see
    // text (report messages, prefixes like "h" & to_hstring(v), filenames).
    assert_pass(
        "single_char_string",
        r#"
library ieee;
use ieee.std_logic_1164.all;
entity tb is end;
architecture rtl of tb is
  signal v : std_logic_vector(7 downto 0) := x"A5";
begin
  process
  begin
    if ("h" & to_hstring(v)) = "hA5" and ("h" & "i") = "hi" then
      report "PASS";
    else
      report "FAIL cat=" & ("h" & to_hstring(v));
    end if;
    wait;
  end process;
end;
"#,
    );
}

#[test]
fn user_to_string_overload_shadows_builtin() {
    // A user-declared `to_string(integer)` with a body shadows the built-in
    // for calls it can bind (LRM 1076 §11.3), but must not steal calls it
    // cannot bind: `to_string(2.5)` has no fitting user overload and takes
    // the built-in real rendering.
    assert_pass(
        "user_to_string",
        r#"
entity tb is end;
architecture rtl of tb is
  function to_string(value : integer) return string is
  begin
    return "user:" & integer'image(value);
  end function;
begin
  process
  begin
    if to_string(5) = "user:5" and to_string(2.5) = "2.5" then
      report "PASS";
    else
      report "FAIL int=" & to_string(5) & " real=" & to_string(2.5);
    end if;
    wait;
  end process;
end;
"#,
    );
}

#[test]
fn delayed_concurrent_assign_waveform() {
    // Waveform delays are each relative to the assignment time.
    assert_pass(
        "delayed_assign",
        r#"
entity tb is end;
architecture rtl of tb is
  signal s : bit := '0';
begin
  s <= '1' after 1 ns, '0' after 2 ns;
  process
  begin
    wait for 500 ps;
    if s = '0' then report "PASS"; else report "FAIL early"; end if;
    wait for 1 ns;
    if s = '1' then report "PASS"; else report "FAIL mid"; end if;
    wait for 2 ns;
    if s = '0' then report "PASS"; else report "FAIL late"; end if;
    wait;
  end process;
end;
"#,
    );
}

#[test]
fn conditional_concurrent_assign() {
    assert_pass(
        "cond_assign",
        r#"
entity tb is end;
architecture rtl of tb is
  signal sel : bit := '1';
  signal y : bit;
begin
  y <= '1' when sel = '1' else '0';
  process
  begin
    wait for 1 ns;
    if y = '1' then report "PASS"; else report "FAIL"; end if;
    sel <= '0';
    wait for 1 ns;
    if y = '0' then report "PASS"; else report "FAIL"; end if;
    wait;
  end process;
end;
"#,
    );
}

#[test]
fn selected_concurrent_assign() {
    assert_pass(
        "selected_assign",
        r#"
entity tb is end;
architecture rtl of tb is
  signal sel : bit_vector(1 downto 0) := "01";
  signal y : bit_vector(3 downto 0);
begin
  with sel select y <=
    "0001" when "00",
    "0010" when "01",
    "1111" when others;
  process
  begin
    wait for 1 ns;
    if y = "0010" then report "PASS"; else report "FAIL"; end if;
    wait;
  end process;
end;
"#,
    );
}
