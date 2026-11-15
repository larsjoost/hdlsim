//! Constrained-random stimulus: `$urandom` / `$urandom_range` draw from the
//! `--seed`-ed kernel RNG (same stream as `$random`), and `$rand_solve` solves
//! a small integer constraint spec and assigns the results to variables.
//! Same seed -> identical stimulus; constraints must hold; unsat specs warn
//! without failing the test.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.env("HDLSIM_NO_CACHE", "1");
    c
}

fn tmp(name: &str, content: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let p = std::env::temp_dir().join(format!("hdlsim_rand_{}_{}_{}", std::process::id(), n, name));
    std::fs::write(&p, content).unwrap();
    p
}

fn run(f: &PathBuf, seed: Option<u64>) -> (bool, String) {
    let mut c = hdlsim();
    c.arg("run").arg("--top").arg("tb");
    if let Some(s) = seed {
        c.arg("--seed").arg(s.to_string());
    }
    let out = c.arg(f).output().expect("runs");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string(),
    )
}

const URANDOM_TB: &str = "module tb;\n    initial begin\n        $display(\"u1=%0d\", $urandom);\n        $display(\"u2=%0d\", $urandom);\n        $display(\"u3=%0d\", $urandom);\n        $finish;\n    end\nendmodule\n";

const RANGE_TB: &str = r#"module tb;
    integer i;
    integer v;
    initial begin
        for (i = 0; i < 200; i = i + 1) begin
            v = $urandom_range(5, 9);
            if (v < 5 || v > 9) $error("range violated");
            v = $urandom_range(7);
            if (v < 0 || v > 7) $error("range violated");
        end
        $display("RANGE_OK");
        $finish;
    end
endmodule
"#;

const SOLVE_TB: &str = r#"module tb;
    integer a;
    integer b;
    initial begin
        $rand_solve("a: 0..10; b: 0..10; a + b == 10; a != b", "a", a, "b", b);
        if (a + b != 10 || a == b) $error("constraint violated");
        $display("solved a=%0d b=%0d", a, b);
        $finish;
    end
endmodule
"#;

const SOLVE_SET_TB: &str = r#"module tb;
    integer op;
    initial begin
        $rand_solve("op: 1, 2, 4, 8; op != 2", "op", op);
        if (op != 1 && op != 4 && op != 8) $error("set violated");
        $display("op=%0d", op);
        $finish;
    end
endmodule
"#;

const UNSAT_TB: &str = r#"module tb;
    integer a;
    initial begin
        $rand_solve("a: 1..1; a == 2", "a", a);
        $display("UNSAT_DONE a=%0d", a);
        $finish;
    end
endmodule
"#;

#[test]
fn urandom_same_seed_same_sequence() {
    let f = tmp("urand.v", URANDOM_TB);
    let (ok_a, a) = run(&f, Some(42));
    let (ok_b, b) = run(&f, Some(42));
    assert!(ok_a && ok_b);
    assert_eq!(a, b);
    assert!(a.contains("u1=") && a.contains("u3="), "{}", a);
    let _ = std::fs::remove_file(&f);
}

#[test]
fn urandom_different_seed_different_sequence() {
    let f = tmp("urand.v", URANDOM_TB);
    let (ok_a, a) = run(&f, Some(42));
    let (ok_b, b) = run(&f, Some(7));
    assert!(ok_a && ok_b);
    assert_ne!(a, b, "seeds 42 and 7 must differ:\n{}\n{}", a, b);
    let _ = std::fs::remove_file(&f);
}

#[test]
fn urandom_range_stays_in_bounds() {
    let f = tmp("range.v", RANGE_TB);
    let (ok, out) = run(&f, Some(42));
    assert!(ok, "range bounds violated:\n{}", out);
    assert!(out.contains("RANGE_OK"), "{}", out);
    let _ = std::fs::remove_file(&f);
}

#[test]
fn rand_solve_assigns_satisfying_values() {
    let f = tmp("solve.v", SOLVE_TB);
    let (ok, out) = run(&f, Some(42));
    assert!(ok, "constraints violated:\n{}", out);
    let line = out.lines().find(|l| l.starts_with("solved ")).expect(&out);
    let nums: Vec<i64> = line
        .split_whitespace()
        .filter_map(|t| {
            t.trim_start_matches("a=")
                .trim_start_matches("b=")
                .parse::<i64>()
                .ok()
        })
        .collect();
    assert_eq!(nums.len(), 2, "{}", line);
    assert_eq!(nums[0] + nums[1], 10, "{}", line);
    assert_ne!(nums[0], nums[1], "{}", line);
    let _ = std::fs::remove_file(&f);
}

#[test]
fn rand_solve_set_domain() {
    let f = tmp("solve_set.v", SOLVE_SET_TB);
    let (ok, out) = run(&f, Some(1));
    assert!(ok, "{}", out);
    assert!(out.contains("op="), "{}", out);
    let _ = std::fs::remove_file(&f);
}

#[test]
fn rand_solve_reproducible_per_seed() {
    let f = tmp("solve.v", SOLVE_TB);
    let (ok_a, a) = run(&f, Some(42));
    let (ok_b, b) = run(&f, Some(42));
    assert!(ok_a && ok_b);
    assert_eq!(a, b);
    let (ok_c, c) = run(&f, Some(7));
    assert!(ok_c);
    assert_ne!(
        a, c,
        "seeds 42 and 7 should pick different solutions:\n{}\n{}",
        a, c
    );
    let _ = std::fs::remove_file(&f);
}

#[test]
fn rand_solve_unsat_warns_but_continues() {
    let f = tmp("unsat.v", UNSAT_TB);
    let (ok, out) = run(&f, Some(42));
    assert!(ok, "a warning must not fail the test:\n{}", out);
    assert!(out.contains("warning: $rand_solve"), "{}", out);
    assert!(out.contains("UNSAT_DONE"), "{}", out);
    let _ = std::fs::remove_file(&f);
}
