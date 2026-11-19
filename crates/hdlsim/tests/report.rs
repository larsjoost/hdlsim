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

/// Regression test for the "bundle not attached to the issue" problem
/// (issue #113): `upload_gist` passed `--secret` to `gh gist create`, but gh
/// has no such flag (secret is the default), so every upload failed with
/// "unknown flag: --secret" and the issue was filed with no bundle link.
/// A fake `gh` on PATH (no network, no real issue) records what hdlsim asks
/// for: the gist upload must use valid flags and stage the bundle under the
/// exact name the issue body links to.
#[test]
fn report_uploads_bundle_as_gist_and_links_it_in_the_issue() {
    let dir = make_failing_project("gist");
    let log = dir.join("gh.log");
    let body_cap = dir.join("issue-body.md");
    let gh = dir.join("gh");
    let log_s = log.display().to_string();
    let body_s = body_cap.display().to_string();
    let script = format!(
        "#!/bin/sh\n\
         echo \"$@\" >> \"{log_s}\"\n\
         last=\"\"\n\
         for a in \"$@\"; do last=\"$a\"; done\n\
         echo \"staged-file: $last\" >> \"{log_s}\"\n\
         if [ \"$1\" = \"gist\" ]; then\n\
           echo \"https://gist.github.com/fakeabc123def\"\n\
         elif [ \"$1\" = \"issue\" ]; then\n\
           prev=\"\"\n\
           for a in \"$@\"; do\n\
             if [ \"$prev\" = \"--body-file\" ]; then cp \"$a\" \"{body_s}\"; fi\n\
             prev=\"$a\"\n\
           done\n\
           echo \"https://github.com/larsjoost/hdlsim/issues/999\"\n\
         fi\n"
    );
    std::fs::write(&gh, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let path = format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = hdlsim()
        .current_dir(&dir)
        .env("PATH", &path)
        .args(["check", "bad.vhd", "--report", "boom: gist upload test"])
        .output()
        .expect("runs");
    assert_eq!(out.status.code(), Some(1), "{:?}", out.status);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("https://gist.github.com/fakeabc123def"),
        "gist upload must be reported:\n{stderr}"
    );
    assert!(
        stderr.contains("issues/999"),
        "issue creation must be reported:\n{stderr}"
    );

    let log = std::fs::read_to_string(&log).unwrap();
    let gist_line = log
        .lines()
        .find(|l| l.starts_with("gist create"))
        .expect("gh gist create must be invoked");
    assert!(
        !gist_line.contains("--secret"),
        "gh has no --secret flag; it must never be passed: {gist_line}"
    );
    assert!(
        !gist_line.contains("--public"),
        "secret is the default; --public only with HDLSIM_REPORT_GIST=public: {gist_line}"
    );
    let staged = log
        .lines()
        .find(|l| l.starts_with("staged-file: "))
        .expect("staged gist file recorded");
    assert!(
        staged.ends_with("hdlsim-repro.tar.b64"),
        "the staged file basename becomes the gist file name and must match the raw link: {staged}"
    );

    let issue = std::fs::read_to_string(&body_cap).expect("issue body captured");
    assert!(
        issue.contains("https://gist.github.com/fakeabc123def"),
        "issue body must link the gist:\n{issue}"
    );
    assert!(
        issue.contains("https://gist.github.com/fakeabc123def/raw/hdlsim-repro.tar.b64"),
        "issue body must link the raw bundle with the exact gist file name:\n{issue}"
    );
    assert!(
        !issue.contains("attach it to this issue"),
        "issue body must not fall back to manual attachment when the gist works:\n{issue}"
    );
    assert!(
        issue.contains("hdlsim check bad.vhd --bundle hdlsim-repro.tar"),
        "{issue}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
