//! `--seed N` (and do-file `seed N`) must make `$random` reproducible:
//! same seed -> identical transcript, different seed -> different values.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

fn tmp(name: &str, content: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let p = std::env::temp_dir().join(format!("hdlsim_seed_{}_{}_{}", std::process::id(), n, name));
    std::fs::write(&p, content).unwrap();
    p
}

const RAND_TB: &str = "module tb;\n    initial begin\n        $display(\"r1=%0d\", $random);\n        $display(\"r2=%0d\", $random);\n        $display(\"r3=%0d\", $random);\n        $finish;\n    end\nendmodule\n";

fn run_with_seed(f: &PathBuf, seed: Option<u64>) -> String {
    let mut c = hdlsim();
    c.arg("run").arg("--top").arg("tb");
    if let Some(s) = seed {
        c.arg("--seed").arg(s.to_string());
    }
    let out = c.arg(f).output().expect("runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test]
fn same_seed_same_random_sequence() {
    let f = tmp("tb.v", RAND_TB);
    let a = run_with_seed(&f, Some(42));
    let b = run_with_seed(&f, Some(42));
    assert_eq!(a, b);
    assert!(a.contains("r1=") && a.contains("r3="), "{}", a);
    let _ = std::fs::remove_file(&f);
}

#[test]
fn different_seed_different_sequence() {
    let f = tmp("tb.v", RAND_TB);
    let a = run_with_seed(&f, Some(42));
    let b = run_with_seed(&f, Some(7));
    assert_ne!(a, b, "seeds 42 and 7 must differ:\n{}\n{}", a, b);
    let _ = std::fs::remove_file(&f);
}

#[test]
fn default_seed_is_deterministic_too() {
    let f = tmp("tb.v", RAND_TB);
    let a = run_with_seed(&f, None);
    let b = run_with_seed(&f, None);
    assert_eq!(a, b);
    let _ = std::fs::remove_file(&f);
}

#[test]
fn do_file_seed_matches_run_seed() {
    let f = tmp("tb.v", RAND_TB);
    let dof = tmp("s.do", "seed 42\nrun -all\nquit\n");
    let out = hdlsim()
        .arg("do")
        .arg(&dof)
        .arg("--top")
        .arg("tb")
        .arg(&f)
        .output()
        .expect("runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(stdout.contains("seed = 42"), "{}", stdout);
    let via_run = run_with_seed(&f, Some(42));
    for line in via_run.lines().filter(|l| l.starts_with('r')) {
        assert!(
            stdout.contains(line),
            "do-file output missing {:?}:\n{}",
            line,
            stdout
        );
    }
    let _ = std::fs::remove_file(&f);
    let _ = std::fs::remove_file(&dof);
}
