//! Reproducer bundles: `hdlsim bundle` packs a project into one .tar file and
//! `hdlsim run --bundle repro.tar` reproduces the run from it — the format a
//! bug report on the hdlsim repo should use.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    Command::new(env!("CARGO_BIN_EXE_hdlsim"))
}

fn tmp_tar(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("hdlsim_test_{}_{}.tar", tag, std::process::id()))
}

#[test]
fn bundle_library_project_roundtrip() {
    // fp-vhdl example: library-only project (hdlsim.ini maps work = .)
    let example = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/fp-vhdl");
    let tar = tmp_tar("fp");
    let _ = std::fs::remove_file(&tar);
    let out = hdlsim()
        .current_dir(&example)
        .args(["bundle", "--out", tar.to_str().unwrap(), "--top", "tb_fp"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "bundle failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(tar.is_file());
    // Run from the bundle alone, from an unrelated cwd — the original example
    // dir is not needed.
    let out = hdlsim()
        .current_dir(std::env::temp_dir())
        .args([
            "run",
            "--bundle",
            tar.to_str().unwrap(),
            "--top",
            "tb_fp",
            "--max-time",
            "100us",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("PASS: fp-vhdl add/mul all 10 cases correct"),
        "run from bundle failed ({:?}):\n{}",
        out.status,
        stdout
    );
    let _ = std::fs::remove_file(&tar);
}

#[test]
fn bundle_explicit_files_roundtrip() {
    // vhdl-counter example: explicit file arguments, no hdlsim.ini
    let example = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/vhdl-counter");
    let tar = tmp_tar("ctr");
    let _ = std::fs::remove_file(&tar);
    let out = hdlsim()
        .current_dir(&example)
        .args([
            "bundle",
            "--out",
            tar.to_str().unwrap(),
            "--top",
            "tb_counter",
            "counter.vhd",
            "tb_counter.vhd",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let out = hdlsim()
        .current_dir(std::env::temp_dir())
        .args([
            "run",
            "--bundle",
            tar.to_str().unwrap(),
            "--top",
            "tb_counter",
            "--max-time",
            "1us",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("PASS"),
        "run from bundle failed ({:?}):\n{}",
        out.status,
        stdout
    );
    let _ = std::fs::remove_file(&tar);
}

#[test]
fn bundle_multilibrary_vhdl_project_roundtrip() {
    // The riscv-vhdl example is a VHDL project split across TWO libraries
    // (hdlsim.ini: rv32i = rtl holds the CPU, work = tb holds the testbenches,
    // which instantiate `entity rv32i.alu` etc.). Bundling must capture both
    // library directories so the bundle alone reproduces the cross-library
    // dependency resolution.
    let example = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/riscv-vhdl");
    let tar = tmp_tar("riscv");
    let _ = std::fs::remove_file(&tar);
    let out = hdlsim()
        .current_dir(&example)
        .args([
            "bundle",
            "--out",
            tar.to_str().unwrap(),
            "--top",
            "tb_cpu_fib",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "bundle failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The archive must contain both libraries plus the generated config.
    let bytes = std::fs::read(&tar).unwrap();
    let entries = hdl_lib::bundle::read_tar(&bytes).unwrap();
    let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
    for expected in [
        "libs/rv32i/cpu.vhd",
        "libs/rv32i/alu.vhd",
        "libs/work/tb_cpu_fib.vhd",
        "libs/work/tb_alu.vhd",
        "hdlsim.ini",
        "hdlsim.ini.orig",
        "REPRO.md",
    ] {
        assert!(paths.contains(&expected), "bundle missing {}", expected);
    }
    let ini = entries.iter().find(|e| e.path == "hdlsim.ini").unwrap();
    let ini_text = String::from_utf8_lossy(&ini.data);
    assert!(ini_text.contains("rv32i = libs/rv32i"), "{}", ini_text);
    assert!(ini_text.contains("work = libs/work"), "{}", ini_text);

    // Run testbenches from the bundle alone, from an unrelated cwd: tb_alu
    // pulls `rv32i.alu` across the library boundary, tb_cpu_fib runs a whole
    // program on the cpu (which pulls alu/decode/regfile/imem/dmem).
    for tb in ["tb_alu", "tb_cpu_fib"] {
        let out = hdlsim()
            .current_dir(std::env::temp_dir())
            .args([
                "run",
                "--bundle",
                tar.to_str().unwrap(),
                "--top",
                tb,
                "--max-time",
                "10us",
            ])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("PASS"),
            "run --bundle --top {} failed ({:?}):\n{}",
            tb,
            out.status,
            stdout
        );
    }
    let _ = std::fs::remove_file(&tar);
}

#[test]
fn bundle_carries_failure_exit_code() {
    // A failing test packed into a bundle must still exit non-zero when run
    // from the bundle.
    let tar = tmp_tar("fail");
    let _ = std::fs::remove_file(&tar);
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tb_fail_err.vhd");
    let out = hdlsim()
        .args([
            "bundle",
            "--out",
            tar.to_str().unwrap(),
            "--top",
            "tb_fail_err",
            fixture.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let out = hdlsim()
        .current_dir(std::env::temp_dir())
        .args([
            "run",
            "--bundle",
            tar.to_str().unwrap(),
            "--top",
            "tb_fail_err",
        ])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "failing test in a bundle must exit non-zero: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let _ = std::fs::remove_file(&tar);
}
