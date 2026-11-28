//! Report/assert transcript lines name the generate-unrolled instance that
//! fired. A for-generate unrolls N copies of the same process (same source
//! line), so the instance path is the only way to tell parallel lanes apart:
//! a process directly in the generate reports `(at lane_gen(0))`, one inside
//! a nested instance `(at lane_gen(0).u_lane)`. VHDL paths use parentheses,
//! Verilog brackets — matching vendor transcripts.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

/// Write `<name>.<ext>` into a per-test temp dir (invented names: the file
/// basename is the `file:` part of transcript locations).
fn write_src(name: &str, ext: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("hdlsim_scope_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(format!("{name}.{ext}"));
    std::fs::write(&p, src).unwrap();
    p
}

fn run(top: &str, file: &PathBuf) -> (String, Option<i32>) {
    let out = hdlsim()
        .args([
            "run",
            "--top",
            top,
            "--max-time",
            "100ns",
            "--max-wall",
            "30s",
            "--no-color",
        ])
        .arg(file)
        .output()
        .expect("runs hdlsim");
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    (text, out.status.code())
}

/// Two lanes, one process per lane, all reporting from the same line.
#[test]
fn generate_process_reports_name_the_lane() {
    let f = write_src(
        "lane_direct",
        "vhd",
        r#"entity lane_direct is end entity;

architecture sim of lane_direct is
  signal en_lane : bit_vector(0 to 1) := (others => '0');
begin
  lane_gen : for i in 0 to 1 generate
    checker : process (en_lane(i))
    begin
      if en_lane(i) = '1' then
        report "lane check failed" severity error;
      end if;
    end process;
  end generate;

  stim : process
  begin
    en_lane <= "11" after 5 ns;
    wait for 20 ns;
    wait;
  end process;
end architecture;
"#,
    );
    let (out, code) = run("lane_direct", &f);
    assert_eq!(code, Some(1), "{out}");
    // Each lane's failure is attributable to its unrolled instance.
    assert!(out.contains("(at lane_gen(0))"), "{out}");
    assert!(out.contains("(at lane_gen(1))"), "{out}");
    let _ = std::fs::remove_file(&f);
}

/// Two unrolled instances of one component, reporting from the same line —
/// the path must name both the generate and the instance label.
#[test]
fn generate_instance_reports_carry_the_full_path() {
    let f = write_src(
        "lane_inst",
        "vhd",
        r#"entity lane_inst is end entity;

architecture sim of lane_inst is
  component leaf_u is
    port (en : in bit);
  end component;

  signal en_lane : bit_vector(0 to 1) := (others => '0');
begin
  lane_gen : for i in 0 to 1 generate
    u_lane : leaf_u
      port map (en => en_lane(i));
  end generate;

  stim : process
  begin
    en_lane <= "11" after 5 ns;
    wait for 20 ns;
    wait;
  end process;
end architecture;

entity leaf_u is
  port (en : in bit);
end entity;

architecture rtl of leaf_u is
begin
  checker : process (en)
  begin
    if en = '1' then
      report "lane check failed" severity error;
    end if;
  end process;
end architecture;
"#,
    );
    let (out, code) = run("lane_inst", &f);
    assert_eq!(code, Some(1), "{out}");
    assert!(out.contains("(at lane_gen(0).u_lane)"), "{out}");
    assert!(out.contains("(at lane_gen(1).u_lane)"), "{out}");
    let _ = std::fs::remove_file(&f);
}

/// Verilog generate lanes: block label + index in brackets, and the nested
/// instance path underneath.
#[test]
fn verilog_generate_lanes_are_named_in_the_transcript() {
    let f = write_src(
        "vlane",
        "v",
        r#"module leaf_v (
  input wire en
);
  always @(en) begin
    if (en) $error("lane check failed");
  end
endmodule

module vlane_tb;
  reg en_lane = 1'b0;

  genvar j;
  generate
    for (j = 0; j < 2; j = j + 1) begin : lane_gen
      leaf_v u_lane (.en(en_lane));
      always @(en_lane) begin
        if (en_lane) $error("block check failed");
      end
    end
  endgenerate

  initial begin
    #5;
    en_lane = 1'b1;
    #5;
    $finish;
  end
endmodule
"#,
    );
    let (out, code) = run("vlane_tb", &f);
    assert_eq!(code, Some(1), "{out}");
    assert!(out.contains("(at lane_gen[0])"), "{out}");
    assert!(out.contains("(at lane_gen[1])"), "{out}");
    assert!(out.contains("(at lane_gen[0].u_lane)"), "{out}");
    assert!(out.contains("(at lane_gen[1].u_lane)"), "{out}");
    let _ = std::fs::remove_file(&f);
}
