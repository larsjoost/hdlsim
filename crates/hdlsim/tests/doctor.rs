//! `hdlsim doctor` reports environment checks and exits 0 when nothing fails;
//! `hdlsim completions <shell>` prints a script covering every subcommand.

use std::process::Command;

fn hdlsim() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hdlsim"));
    c.arg("--no-cache");
    c
}

#[test]
fn doctor_passes_in_a_clean_environment() {
    let out = hdlsim().arg("doctor").output().expect("runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("stdlib"), "{}", stdout);
    assert!(stdout.contains("stdlib units"), "{}", stdout);
    assert!(stdout.contains("0 fail"), "{}", stdout);
}

#[test]
fn doctor_rejects_arguments() {
    let out = hdlsim().args(["doctor", "extra"]).output().expect("runs");
    assert!(!out.status.success());
}

#[test]
fn completions_cover_all_subcommands_for_each_shell() {
    for shell in ["bash", "zsh", "fish"] {
        let out = hdlsim()
            .args(["completions", shell])
            .output()
            .expect("runs");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let s = String::from_utf8_lossy(&out.stdout);
        for cmd in [
            "run",
            "check",
            "lsp",
            "fmt",
            "lint",
            "doctor",
            "completions",
            "regress",
        ] {
            assert!(s.contains(cmd), "{} script missing {}: {}", shell, cmd, s);
        }
    }
}

#[test]
fn completions_reject_unknown_shell() {
    let out = hdlsim()
        .args(["completions", "powershell"])
        .output()
        .expect("runs");
    assert!(!out.status.success());
}
