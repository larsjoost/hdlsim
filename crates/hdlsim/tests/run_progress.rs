//! End-to-end tests for `--progress` simulation ticks: with `--max-time`,
//! one stderr line per decile of the budget the simulated time crosses
//! (`hdlsim: sim t=200ns / 400ns (50%)`); without it, wall-clock
//! rate-limited ticks. Format, exact line content, and ordering against
//! the final summary are asserted — never wall time.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

fn tmp(name: &str, content: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("hdlsim_progress_{}_{}", std::process::id(), name));
    std::fs::write(&p, content).unwrap();
    p
}

/// Run `hdlsim` with stdout and stderr merged into one file (two O_APPEND
/// handles share the end-of-file position), so the ticks' order against the
/// stdout summary is observable. `.output()` would capture the streams
/// separately and lose exactly the ordering these tests pin.
fn run_merged(tag: &str, args: &[&str]) -> (String, ExitStatus) {
    let out = std::env::temp_dir().join(format!(
        "hdlsim_progress_out_{}_{}.txt",
        std::process::id(),
        tag
    ));
    let _ = std::fs::remove_file(&out);
    let a = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&out)
        .expect("open merged output");
    let b = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&out)
        .expect("open merged output");
    let status = hdlsim()
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(a))
        .stderr(Stdio::from(b))
        .status()
        .expect("runs");
    (
        std::fs::read_to_string(&out).expect("read merged output"),
        status,
    )
}

/// Tick lines in stream order.
fn ticks(merged: &str) -> Vec<&str> {
    merged
        .lines()
        .filter(|l| l.starts_with("hdlsim: sim t="))
        .collect()
}

/// The last tick must come before the final summary line.
fn assert_ticks_precede_summary(merged: &str, summary: &str) {
    let summary_at = merged
        .find(summary)
        .unwrap_or_else(|| panic!("summary '{summary}' missing in:\n{merged}"));
    let last_tick_at = merged.rfind("hdlsim: sim t=").expect("at least one tick");
    assert!(last_tick_at < summary_at, "tick after summary:\n{merged}");
}

/// An endless wait loop: simulated time only stops when --max-time cuts
/// the run off.
const LOOP_TB: &str = "entity progress_tb is end entity;\narchitecture sim of progress_tb is\nbegin\n  process\n  begin\n    while true loop\n      wait for 100 ns;\n    end loop;\n  end process;\nend architecture;\n";

/// Same loop, but ends itself after 2.1 us via std.env.finish.
const FINISH_TB: &str = "library std;\nuse std.env.all;\nentity progress_tb is end entity;\narchitecture sim of progress_tb is\nbegin\n  process\n  begin\n    for i in 1 to 21 loop\n      wait for 100 ns;\n    end loop;\n    report \"PROGRESS-TB-DONE\";\n    std.env.finish;\n    wait;\n  end process;\nend architecture;\n";

#[test]
fn max_time_ticks_deciles_then_stops_at_the_budget() {
    let tb = tmp("loop_tb.vhd", LOOP_TB);
    // Deciles of 400 ns are 40 ns apart; the events land on every 100 ns,
    // so the ticks report where the sim is (not each boundary) and the
    // percentages are exact.
    let (merged, status) = run_merged(
        "deciles",
        &[
            "run",
            tb.to_str().unwrap(),
            "--top",
            "progress_tb",
            "--max-time",
            "400ns",
            "--progress",
        ],
    );
    assert!(status.success(), "{}", merged);
    assert_eq!(
        ticks(&merged),
        vec![
            "hdlsim: sim t=100ns / 400ns (25%)",
            "hdlsim: sim t=200ns / 400ns (50%)",
            "hdlsim: sim t=300ns / 400ns (75%)",
            "hdlsim: sim t=400ns / 400ns (100%)",
        ],
        "merged:\n{merged}"
    );
    // The final tick must agree with the stop line, not contradict it:
    // 100% of the budget and "stopped at t=400ns" say the same thing.
    assert_ticks_precede_summary(&merged, "hdlsim: simulation stopped at t=400ns");
    let _ = std::fs::remove_file(&tb);
}

#[test]
fn finish_before_the_budget_stops_the_ticks() {
    let tb = tmp("finish_tb.vhd", FINISH_TB);
    // The tb ends at 2.1 us of a 5 us budget: the ticks end at 40% and
    // nothing follows the finish — no tick may outlive the run.
    let (merged, status) = run_merged(
        "finish",
        &[
            "run",
            tb.to_str().unwrap(),
            "--top",
            "progress_tb",
            "--max-time",
            "5us",
            "--progress",
        ],
    );
    assert!(status.success(), "{}", merged);
    assert!(merged.contains("PROGRESS-TB-DONE"), "{}", merged);
    assert_eq!(
        ticks(&merged),
        vec![
            "hdlsim: sim t=500ns / 5us (10%)",
            "hdlsim: sim t=1us / 5us (20%)",
            "hdlsim: sim t=1.5us / 5us (30%)",
            "hdlsim: sim t=2us / 5us (40%)",
        ],
        "merged:\n{merged}"
    );
    assert_ticks_precede_summary(&merged, "hdlsim: simulation stopped at t=2.1us");
    let _ = std::fs::remove_file(&tb);
}

#[test]
fn without_progress_no_tick_lines() {
    let tb = tmp("quiet_tb.vhd", LOOP_TB);
    let out = hdlsim()
        .args([
            "run",
            tb.to_str().unwrap(),
            "--top",
            "progress_tb",
            "--max-time",
            "400ns",
        ])
        .output()
        .expect("runs");
    assert!(out.status.success());
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    // Opt-in: the default output carries no tick lines at all.
    assert!(!all.contains("hdlsim: sim t="), "{}", all);
    let _ = std::fs::remove_file(&tb);
}

#[test]
fn restored_run_resumes_ticks_from_the_checkpoint() {
    let tb = tmp("ckpt_tb.vhd", LOOP_TB);
    let ck = std::env::temp_dir().join(format!("hdlsim_progress_{}_ck.bin", std::process::id()));
    let _ = std::fs::remove_file(&ck);
    checkpoint_leg(&tb, &ck);
    restore_leg(&tb, &ck);
    let _ = std::fs::remove_file(&tb);
    let _ = std::fs::remove_file(&ck);
}

/// Leg 1: stop at the 400 ns budget and checkpoint; the ticks cover it.
fn checkpoint_leg(tb: &Path, ck: &Path) {
    let (m1, s1) = run_merged(
        "ckpt1",
        &[
            "run",
            tb.to_str().unwrap(),
            "--top",
            "progress_tb",
            "--max-time",
            "400ns",
            "--checkpoint",
            ck.to_str().unwrap(),
            "--progress",
        ],
    );
    assert!(s1.success(), "{}", m1);
    assert!(m1.contains("checkpoint written"), "{}", m1);
    assert_eq!(
        ticks(&m1),
        vec![
            "hdlsim: sim t=100ns / 400ns (25%)",
            "hdlsim: sim t=200ns / 400ns (50%)",
            "hdlsim: sim t=300ns / 400ns (75%)",
            "hdlsim: sim t=400ns / 400ns (100%)",
        ],
        "merged:\n{m1}"
    );
}

/// Leg 2: resume with a 1 us budget — only the ticks ahead of the restored
/// time (400 ns) may print, continuing in order. The deciles already
/// crossed in the checkpoint leg must not re-tick.
fn restore_leg(tb: &Path, ck: &Path) {
    let (m2, s2) = run_merged(
        "ckpt2",
        &[
            "run",
            tb.to_str().unwrap(),
            "--top",
            "progress_tb",
            "--restore",
            ck.to_str().unwrap(),
            "--max-time",
            "1us",
            "--progress",
        ],
    );
    assert!(s2.success(), "{}", m2);
    assert_eq!(
        ticks(&m2),
        vec![
            "hdlsim: sim t=500ns / 1us (50%)",
            "hdlsim: sim t=600ns / 1us (60%)",
            "hdlsim: sim t=700ns / 1us (70%)",
            "hdlsim: sim t=800ns / 1us (80%)",
            "hdlsim: sim t=900ns / 1us (90%)",
            "hdlsim: sim t=1us / 1us (100%)",
        ],
        "merged:\n{m2}"
    );
    assert_ticks_precede_summary(&m2, "hdlsim: simulation stopped at t=1us");
}

#[test]
fn without_max_time_a_short_run_prints_no_wall_tick() {
    let tb = tmp("wall_tb.vhd", FINISH_TB);
    // Wall-clock ticks are rate-limited to >= 500 ms apart, so a run that
    // finishes in milliseconds stays silent — deterministic decile ticks
    // are the testable path, and this pins the quiet one.
    let (merged, status) = run_merged(
        "wall",
        &[
            "run",
            tb.to_str().unwrap(),
            "--top",
            "progress_tb",
            "--progress",
        ],
    );
    assert!(status.success(), "{}", merged);
    assert!(ticks(&merged).is_empty(), "merged:\n{merged}");
    assert!(merged.contains("PROGRESS-TB-DONE"), "{}", merged);
    let _ = std::fs::remove_file(&tb);
}
