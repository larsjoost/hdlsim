//! `--max-mem-mb` memory guard: a run over the limit must stop with the
//! actionable diagnostic (exit 1) instead of being OOM-killed (exit 137, no
//! output), and every run must report per-phase time/peak-RSS on stderr.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn phases_report_time_and_peak_rss() {
    let out = hdlsim()
        .args(["run", "--top", "tb_named_assoc"])
        .arg(fixture("vhdl_named_assoc_tb.vhd"))
        .output()
        .expect("runs");
    assert!(out.status.success(), "{}", out.status);
    let stderr = String::from_utf8_lossy(&out.stderr);
    for phase in ["compile", "elaborate", "sim-init", "simulate"] {
        assert!(
            stderr.contains(&format!("hdlsim: {phase} ")),
            "missing {phase} phase line in:\n{stderr}"
        );
    }
    assert!(
        stderr.contains("peak-rss"),
        "missing peak-rss in:\n{stderr}"
    );
}

#[test]
fn mem_limit_stops_the_run_with_a_diagnostic() {
    // 1 MiB: every run exceeds it, so the boundary check fires deterministically.
    let out = hdlsim()
        .args(["--max-mem-mb", "1"])
        .args(["run", "--top", "tb_named_assoc"])
        .arg(fixture("vhdl_named_assoc_tb.vhd"))
        .output()
        .expect("runs");
    assert_eq!(
        out.status.code(),
        Some(1),
        "expected graceful exit 1, got {:?}",
        out.status
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("memory limit exceeded"),
        "expected the memory-limit diagnostic in:\n{stderr}"
    );
    assert!(
        stderr.contains("mitigate"),
        "expected mitigation hints in:\n{stderr}"
    );
}
