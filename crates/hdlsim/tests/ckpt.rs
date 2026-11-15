//! End-to-end tests for checkpoint/restore: `hdlsim run --checkpoint` /
//! `--restore` must produce the same result as an uninterrupted run, a
//! checkpoint must refuse to restore against a different design, and the
//! do-file `checkpoint`/`restore` commands must work mid-script.

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
    std::env::temp_dir().join(format!("hdlsim_ckpt_{}_{}", std::process::id(), name))
}

fn run_counter(extra: &[&str]) -> std::process::Output {
    let ex = examples().join("counter");
    let tb = ex.join("tb_counter.v");
    let mut args: Vec<String> = vec![
        "run".to_string(),
        "--top".to_string(),
        "tb_counter".to_string(),
        "-L".to_string(),
        format!("work={}", ex.display()),
        tb.to_str().unwrap().to_string(),
    ];
    args.extend(extra.iter().map(|s| s.to_string()));
    hdlsim().args(&args).output().expect("runs")
}

#[test]
fn checkpoint_then_restore_matches_uninterrupted_run() {
    let baseline = run_counter(&[]);
    assert!(baseline.status.success());
    let base_out = String::from_utf8_lossy(&baseline.stdout);
    assert!(base_out.contains("PASS"), "{}", base_out);
    assert!(base_out.contains("t=116000000 fs"), "{}", base_out);

    let ck = tmp("ck.bin");
    let _ = std::fs::remove_file(&ck);
    let part = run_counter(&["--max-time", "50ns", "--checkpoint", ck.to_str().unwrap()]);
    assert!(
        part.status.success(),
        "{}",
        String::from_utf8_lossy(&part.stderr)
    );
    let part_out = String::from_utf8_lossy(&part.stdout);
    assert!(part_out.contains("t=50000000 fs"), "{}", part_out);
    assert!(part_out.contains("checkpoint written"), "{}", part_out);
    assert!(ck.is_file());

    let resumed = run_counter(&["--restore", ck.to_str().unwrap()]);
    assert!(
        resumed.status.success(),
        "{}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    let res_out = String::from_utf8_lossy(&resumed.stdout);
    assert!(res_out.contains("PASS"), "{}", res_out);
    assert!(res_out.contains("t=116000000 fs"), "{}", res_out);
    let _ = std::fs::remove_file(&ck);
}

#[test]
fn restore_rejects_mismatched_design() {
    let ck = tmp("ck2.bin");
    let _ = std::fs::remove_file(&ck);
    let part = run_counter(&["--max-time", "50ns", "--checkpoint", ck.to_str().unwrap()]);
    assert!(part.status.success());

    // Restore the Verilog checkpoint against the VHDL design: must fail.
    let dir = examples().join("vhdl-counter");
    let tb = dir.join("tb_counter.vhd");
    let out = hdlsim()
        .args([
            "run",
            "--top",
            "tb_counter",
            "-L",
            &format!("work={}", dir.display()),
            tb.to_str().unwrap(),
            "--restore",
            ck.to_str().unwrap(),
        ])
        .output()
        .expect("runs");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("fingerprint"), "{}", stderr);
    let _ = std::fs::remove_file(&ck);
}

#[test]
fn restore_rejects_garbage_file() {
    let bad = tmp("garbage.bin");
    std::fs::write(&bad, b"not a checkpoint").unwrap();
    let out = run_counter(&["--restore", bad.to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("bad magic"));
    let _ = std::fs::remove_file(&bad);
}

#[test]
fn do_file_checkpoint_and_restore() {
    let ck = tmp("ck3.bin");
    let _ = std::fs::remove_file(&ck);
    let script = tmp("run.do");
    std::fs::write(
        &script,
        format!(
            "run 50ns\ncheckpoint {}\nrestart\nrun 50ns\nrestore {}\nrun -all\nexamine q\n",
            ck.display(),
            ck.display()
        ),
    )
    .unwrap();
    let ex = examples().join("counter");
    let tb = ex.join("tb_counter.v");
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
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("checkpoint written"), "{}", stdout);
    assert!(stdout.contains("restored checkpoint"), "{}", stdout);
    assert!(stdout.contains("PASS"), "{}", stdout);
    assert!(stdout.contains("q = 0x0A"), "{}", stdout);
    let _ = std::fs::remove_file(&ck);
    let _ = std::fs::remove_file(&script);
}
