//! End-to-end tests for the `.hwf` wave format: `run --hwf` writes a compact
//! binary wave file, and `hdlsim wave file.hwf` renders it into the HTML
//! viewer without re-running the simulation.

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
    std::env::temp_dir().join(format!("hdlsim_wave_{}_{}", std::process::id(), name))
}

#[test]
fn hwf_roundtrip_through_html_viewer() {
    let ex = examples().join("counter");
    let tb = ex.join("tb_counter.v");
    let hwf = tmp("w.hwf");
    let html = tmp("w.html");
    let _ = std::fs::remove_file(&hwf);
    let _ = std::fs::remove_file(&html);

    let out = hdlsim()
        .args([
            "run",
            "--top",
            "tb_counter",
            "-L",
            &format!("work={}", ex.display()),
            tb.to_str().unwrap(),
            "--hwf",
            hwf.to_str().unwrap(),
        ])
        .output()
        .expect("runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(hwf.is_file());
    let bytes = std::fs::read(&hwf).unwrap();
    assert_eq!(&bytes[..8], b"HDLHWF01", "hwf magic");
    // Binary and much smaller than the equivalent VCD would be for many events.
    assert!(bytes.len() < 4096, "hwf is compact: {} bytes", bytes.len());

    let out = hdlsim()
        .args(["wave", hwf.to_str().unwrap(), "-o", html.to_str().unwrap()])
        .output()
        .expect("runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("5 signals"), "{}", stdout);
    let page = std::fs::read_to_string(&html).unwrap();
    assert!(page.contains("<title>hdlsim waves"), "html viewer title");
    assert!(page.contains("clk"), "signal name in page");
    assert!(page.contains("PASS"), "transcript messages replayed");

    let _ = std::fs::remove_file(&hwf);
    let _ = std::fs::remove_file(&html);
}

#[test]
fn wave_rejects_garbage_and_vhdl_works() {
    // garbage input -> exit 2 (dispatch Err)
    let bad = tmp("bad.hwf");
    std::fs::write(&bad, b"not a wave file").unwrap();
    let out = hdlsim()
        .args(["wave", bad.to_str().unwrap()])
        .output()
        .expect("runs");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("bad magic"));
    let _ = std::fs::remove_file(&bad);

    // VHDL design through the same path.
    let dir = examples().join("vhdl-counter");
    let tb = dir.join("tb_counter.vhd");
    let hwf = tmp("w2.hwf");
    let html = tmp("w2.html");
    let _ = std::fs::remove_file(&hwf);
    let _ = std::fs::remove_file(&html);
    let out = hdlsim()
        .args([
            "run",
            "--top",
            "tb_counter",
            "-L",
            &format!("work={}", dir.display()),
            tb.to_str().unwrap(),
            "--hwf",
            hwf.to_str().unwrap(),
        ])
        .output()
        .expect("runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = hdlsim()
        .args(["wave", hwf.to_str().unwrap(), "-o", html.to_str().unwrap()])
        .output()
        .expect("runs");
    assert!(out.status.success());
    let page = std::fs::read_to_string(&html).unwrap();
    assert!(page.contains("q"), "vhdl signal in page");
    let _ = std::fs::remove_file(&hwf);
    let _ = std::fs::remove_file(&html);
}
