//! End-to-end tests for `hdlsim gen tb`: generate a testbench skeleton for
//! the example counter in both languages, then prove the generated file
//! round-trips by compiling it together with the DUT via `hdlsim check`.

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

fn tmp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("hdlsim_gen_{}_{}", std::process::id(), name))
}

#[test]
fn gen_tb_verilog_roundtrips_through_check() {
    let counter = examples().join("counter/counter.v");
    let out_file = tmp("tb_counter.v");
    let out = hdlsim()
        .args([
            "gen",
            "tb",
            "--top",
            "counter",
            counter.to_str().unwrap(),
            "-o",
            out_file.to_str().unwrap(),
        ])
        .output()
        .expect("runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let src = std::fs::read_to_string(&out_file).unwrap();
    assert!(src.contains("module tb_counter;"), "{}", src);
    assert!(src.contains("counter dut ("), "{}", src);
    assert!(src.contains(".clk(clk)"), "{}", src);
    assert!(src.contains("always #5 clk = ~clk;"), "{}", src);
    assert!(src.contains("$finish;"), "{}", src);

    // The generated testbench must compile together with the DUT.
    let chk = hdlsim()
        .args([
            "check",
            counter.to_str().unwrap(),
            out_file.to_str().unwrap(),
        ])
        .output()
        .expect("runs");
    assert!(
        chk.status.success(),
        "generated tb does not compile: {}",
        String::from_utf8_lossy(&chk.stderr)
    );
    let _ = std::fs::remove_file(&out_file);
}

#[test]
fn gen_tb_vhdl_roundtrips_through_check() {
    let dir = examples().join("vhdl-counter");
    let counter = dir.join("counter.vhd");
    let out_file = tmp("tb_counter.vhd");
    let out = hdlsim()
        .args([
            "gen",
            "tb",
            "--top",
            "counter",
            "-L",
            &format!("work={}", dir.display()),
            counter.to_str().unwrap(),
            "-o",
            out_file.to_str().unwrap(),
        ])
        .output()
        .expect("runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let src = std::fs::read_to_string(&out_file).unwrap();
    assert!(src.contains("entity tb_counter is"), "{}", src);
    assert!(src.contains("dut : entity work.counter"), "{}", src);
    assert!(src.contains("std_logic_vector(7 downto 0)"), "{}", src);
    assert!(src.contains("std.env.finish;"), "{}", src);

    let chk = hdlsim()
        .args([
            "check",
            "-L",
            &format!("work={}", dir.display()),
            counter.to_str().unwrap(),
            out_file.to_str().unwrap(),
        ])
        .output()
        .expect("runs");
    assert!(
        chk.status.success(),
        "generated tb does not compile: {}",
        String::from_utf8_lossy(&chk.stderr)
    );
    let _ = std::fs::remove_file(&out_file);
}

#[test]
fn gen_tb_lang_override_and_errors() {
    let counter = examples().join("counter/counter.v");
    // --lang vhdl forces a VHDL skeleton even for a Verilog DUT.
    let out = hdlsim()
        .args([
            "gen",
            "tb",
            "--top",
            "counter",
            "--lang",
            "vhdl",
            counter.to_str().unwrap(),
        ])
        .output()
        .expect("runs");
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("entity tb_counter is"));

    // Unknown top: exit 1 with an error message.
    let out = hdlsim()
        .args([
            "gen",
            "tb",
            "--top",
            "does_not_exist",
            counter.to_str().unwrap(),
        ])
        .output()
        .expect("runs");
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("does_not_exist"));

    // Missing subcommand: usage error.
    let out = hdlsim().args(["gen"]).output().expect("runs");
    assert!(out.status.code() != Some(0));
}
