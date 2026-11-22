//! Regression tests: interrupting a run reports where the simulation was.
//! `--max-wall` and SIGINT/SIGTERM must stop the kernel and dump the active
//! process origin (file:line) and current time — not die silently, and not
//! exit 0 as if the run had completed.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.env("HDLSIM_NO_CACHE", "1");
    c
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/{}", name))
}

/// `--max-wall 0.3` stops the endless design, exits 124 (timeout
/// convention) and names the process that was running.
#[test]
fn wall_budget_stops_and_reports_location() {
    let out = hdlsim()
        .args(["run", "--top", "endless_tb", "--max-wall", "0.3"])
        .arg(fixture("endless_tb.vhd"))
        .output()
        .expect("runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let code = out.status.code().expect("exits by signal only on crash");
    assert_eq!(code, 124, "wall budget exit code, output:\n{stdout}\n{stderr}");
    assert!(
        stdout.contains("wall-time budget of 0.3s exceeded"),
        "must name the reason:\n{stdout}\n{stderr}"
    );
    assert!(
        stdout.contains("endless_tb.vhd:"),
        "must name the active process origin:\n{stdout}\n{stderr}"
    );
}

/// SIGINT stops the kernel, dumps the location, and exits 130 (shell
/// convention for a signal death) — not 0, which would read as a pass.
#[cfg(unix)]
#[test]
fn sigint_stops_and_reports_location() {
    let child = hdlsim()
        .args(["run", "--top", "endless_tb"])
        .arg(fixture("endless_tb.vhd"))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawns");
    // The tiny design elaborates in well under 400 ms; the sim is grinding
    // on `wait for 1 us` events when the signal lands.
    std::thread::sleep(std::time::Duration::from_millis(400));
    let status = Command::new("sh")
        .arg("-c")
        .arg(format!("kill -INT {}", child.id()))
        .status()
        .expect("signals");
    assert!(status.success(), "kill -INT delivered");
    let out = child.wait_with_output().expect("child exits");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let code = out.status.code().expect("interrupt path exits by code");
    assert_eq!(code, 130, "SIGINT exit code, output:\n{stdout}\n{stderr}");
    assert!(
        stdout.contains("interrupt received"),
        "must name the reason:\n{stdout}\n{stderr}"
    );
    assert!(
        stdout.contains("endless_tb.vhd:"),
        "must name the active process origin:\n{stdout}\n{stderr}"
    );
}
