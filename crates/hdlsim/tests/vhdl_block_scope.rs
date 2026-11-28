//! Regression tests for simulator guards and block scoping found while
//! verifying vendor testbenches:
//!
//! * block declarations are lexically scoped (LRM 11.2) — a block-local signal
//!   shadows an outer one, entity ports stay visible in block sensitivity
//!   lists, and `process(all)` matches case-insensitively;
//! * a `loop` with no `wait` is stopped by the interpreter step budget
//!   (Modelsim's "iteration limit") with a diagnostic naming the process;
//! * a design that keeps producing zero-delay events is stopped by the
//!   delta-cycle limit;
//! * guard-stopped runs exit nonzero so CI does not read them as passes;
//! * a for-loop parameter is scoped to its loop: concurrent loops with the
//!   same parameter name each run their own iteration count, and a loop
//!   parameter never writes an outer object of the same name.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/{}", name))
}

#[test]
fn block_scope_shadows_and_sees_entity_ports() {
    let out = hdlsim()
        .args(["run", "--top", "tb_block_shadow", "--max-time", "1us"])
        .arg(fixture("vhdl_block_shadow_tb.vhd"))
        .output()
        .expect("runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "expected exit 0, got {:?}\n{}{}",
        out.status,
        stdout,
        stderr
    );
    assert!(stdout.contains("PASS: block scope shadowing"), "{}", stdout);
    assert!(!stderr.contains("E0401"), "{}", stderr);
}

#[test]
fn runaway_loop_is_stopped_with_diagnostic() {
    let out = hdlsim()
        .args(["--max-proc-steps", "50000"])
        .args(["run", "--top", "tb_runaway", "--max-time", "1ms"])
        .arg(fixture("vhdl_runaway_loop_tb.vhd"))
        .output()
        .expect("runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !out.status.success(),
        "guard-stopped run must exit nonzero: {}",
        stdout
    );
    assert!(stdout.contains("process step limit exceeded"), "{}", stdout);
    // The diagnostic names the runaway process by source location.
    assert!(
        stdout.contains("vhdl_runaway_loop_tb.vhd"),
        "names the source: {}",
        stdout
    );
    assert!(stdout.contains("loops without waiting"), "{}", stdout);
}

#[test]
fn delta_cycle_loop_is_stopped_with_diagnostic() {
    // `wait for 0 ns` in a loop: every iteration schedules a wake at the same
    // simulation time — the delta-cycle limit must stop it (same-time event
    // batches count against the budget; only real time advances reset it).
    let dir = std::env::temp_dir().join("hdlsim_delta_loop_tb.vhd");
    std::fs::write(
        &dir,
        "entity tb_delta_loop is\nend entity;\narchitecture sim of tb_delta_loop is\nbegin\n  p : PROCESS IS\n  BEGIN\n    LOOP\n      WAIT FOR 0 ns;\n    END LOOP;\n  END PROCESS p;\nend architecture;\n",
    )
    .expect("write fixture");
    let out = hdlsim()
        .args(["run", "--top", "tb_delta_loop", "--max-time", "1ms"])
        .arg(&dir)
        .output()
        .expect("runs");
    let _ = std::fs::remove_file(&dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !out.status.success(),
        "delta-loop run must exit nonzero: {}",
        stdout
    );
    assert!(stdout.contains("delta-cycle limit exceeded"), "{}", stdout);
}

#[test]
fn guards_stop_quickly_not_hang() {
    // Both runaway fixtures must terminate promptly (the guards exist so an
    // AI-driven session cannot hang on a broken testbench).
    let start = std::time::Instant::now();
    let out = hdlsim()
        .args(["--max-proc-steps", "20000"])
        .args(["run", "--top", "tb_runaway", "--max-time", "1ms"])
        .arg(fixture("vhdl_runaway_loop_tb.vhd"))
        .output()
        .expect("runs");
    assert!(
        start.elapsed() < Duration::from_secs(30),
        "step budget did not stop the run in time"
    );
    assert!(!out.status.success());
}

#[test]
fn for_loop_parameter_is_scoped_to_its_loop() {
    let out = hdlsim()
        .args(["run", "--top", "tb_for_param_scope", "--max-time", "2us"])
        .arg(fixture("vhdl_for_param_scope_tb.vhd"))
        .output()
        .expect("runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "expected exit 0, got {:?}\n{}{}",
        out.status,
        stdout,
        stderr
    );
    for pass in ["PASS gen_a", "PASS gen_b", "PASS nest", "PASS guard"] {
        assert!(stdout.contains(pass), "missing `{}`: {}", pass, stdout);
    }
}
