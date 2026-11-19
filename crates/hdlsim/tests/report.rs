//! `--report <msg>` / `--report-dry-run`: on a failed command, hdlsim offers to
//! file a GitHub issue with a reproducer bundle. Tests only exercise the
//! dry-run path — no issue is ever created from CI (no network, no token use).

use std::path::PathBuf;
use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.env("HDLSIM_NO_CACHE", "1");
    c
}

fn make_failing_project(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("hdlsim_report_{}_{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("bad.vhd"),
        "entity bad is end entity;\narchitecture rtl of bad is\n  signal q : bit;\nbegin\n  q <= unknown_sig;\nend architecture;\n",
    )
    .unwrap();
    dir
}

#[test]
fn report_dry_run_prints_issue_without_uploading() {
    let dir = make_failing_project("dry");
    let out = hdlsim()
        .args([
            "check",
            "bad.vhd",
            "--report",
            "check fails on unknown signal",
            "--report-dry-run",
        ])
        .current_dir(&dir)
        .output()
        .expect("runs");
    // The original failure exit code is preserved.
    assert_eq!(out.status.code(), Some(1), "{:?}", out.status);
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(all.contains("dry run"), "{}", all);
    assert!(all.contains("no issue"), "{}", all);
    assert!(all.contains("gist"), "{}", all);
    assert!(all.contains("larsjoost/hdlsim"), "{}", all);
    assert!(all.contains("check fails on unknown signal"), "{}", all);
    assert!(all.contains("gh issue create"), "{}", all);
    // A real upload would print an issue URL like .../issues/123 — dry-run must not.
    assert!(!all.contains("/issues/1"), "{}", all);
    // The reproducer bundle was written and contains the failing source.
    let tar_line = all
        .lines()
        .find(|l| l.contains("reproducer bundle:"))
        .expect("bundle line");
    let tar = PathBuf::from(
        tar_line
            .split("reproducer bundle:")
            .nth(1)
            .unwrap()
            .trim()
            .split_whitespace()
            .next()
            .unwrap(),
    );
    assert!(tar.is_file(), "{} ({:?})", tar.display(), all);
    let listing = Command::new("tar")
        .args(["-tf"])
        .arg(&tar)
        .output()
        .expect("tar");
    let names = String::from_utf8_lossy(&listing.stdout);
    assert!(names.contains("bad.vhd"), "{}", names);
    assert!(!names.contains(".hdlsim-cache"), "{}", names);
    // The drafted issue body carries the exit code, the explained diagnostic
    // codes, and the exact reproduce command — everything a fixer needs.
    let body_line = all
        .lines()
        .find(|l| l.contains("issue body:"))
        .expect("body line");
    let body_path = PathBuf::from(body_line.split("issue body:").nth(1).unwrap().trim());
    let body = std::fs::read_to_string(&body_path).expect("body file");
    assert!(body.contains("exit code: 1"), "{}", body);
    assert!(body.contains("## Diagnostic codes"), "{}", body);
    assert!(body.contains("`E0401`"), "{}", body);
    assert!(body.contains("## Reproduce from the bundle"), "{}", body);
    assert!(
        body.contains("hdlsim check bad.vhd --bundle hdlsim-repro.tar"),
        "{}",
        body
    );
}

#[test]
fn without_report_only_the_hint_is_printed() {
    let dir = make_failing_project("plain");
    let out = hdlsim()
        .args(["check", "bad.vhd"])
        .current_dir(&dir)
        .output()
        .expect("runs");
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("issues/new"), "{}", stderr);
    assert!(stderr.contains("--report"), "{}", stderr);
    assert!(!stderr.contains("hdlsim report:"), "{}", stderr);
}
