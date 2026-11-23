//! End-to-end tests for reverse debug in do-files: after each `run`, an
//! in-memory checkpoint is pushed; `back [N]` restores the earlier state.

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

fn examples() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples"))
}

fn tmp(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "hdlsim_rev_{}_{}_{}",
        std::process::id(),
        name,
        N.fetch_add(1, Ordering::Relaxed)
    ))
}

fn run_do(script_body: &str) -> std::process::Output {
    let ex = examples().join("counter");
    let tb = ex.join("tb_counter.v");
    let script = tmp("rev");
    std::fs::write(&script, script_body).unwrap();
    let out = hdlsim()
        .args([
            "do",
            script.to_str().unwrap(),
            "--top",
            "tb_counter",
            "-L",
            &format!("work={}", ex.display()),
            tb.to_str().unwrap(),
        ])
        .output()
        .expect("runs");
    let _ = std::fs::remove_file(&script);
    out
}

#[test]
fn back_restores_earlier_state() {
    let out =
        run_do("run 30ns\nexamine q\nrun 30ns\nexamine q\nback\nexamine q\nrun 30ns\nexamine q\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let q_values: Vec<&str> = stdout.lines().filter(|l| l.starts_with("q = ")).collect();
    // q@30ns, q@60ns, q after back (=q@30ns), q after forward run (=q@60ns)
    assert_eq!(q_values.len(), 4, "{}", stdout);
    assert_eq!(
        q_values[0], q_values[2],
        "back restores earlier value: {}",
        stdout
    );
    assert_eq!(q_values[1], q_values[3], "forward again: {}", stdout);
    assert_ne!(
        q_values[0], q_values[1],
        "time actually advanced: {}",
        stdout
    );
    assert!(stdout.contains("back to t=30ns"), "{}", stdout);
}

#[test]
fn back_multiple_steps_and_history_limit() {
    let out = run_do("run 30ns\nrun 30ns\nrun 30ns\nback 2\nexamine q\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("back to t=30ns"), "{}", stdout);
    assert!(stdout.contains("q = "), "{}", stdout);

    // More steps than history -> error, non-zero exit.
    let out = run_do("run 30ns\nback 5\n");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("history"));
}

#[test]
fn restart_clears_history() {
    let out = run_do("run 30ns\nrestart\nback\n");
    assert!(!out.status.success(), "back with empty history must fail");
    assert!(String::from_utf8_lossy(&out.stderr).contains("history"));
}
