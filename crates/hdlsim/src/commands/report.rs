//! `--report <msg>` — when a command fails, file a GitHub issue with a
//! reproducer bundle attached (base64 in the issue body) instead of only
//! printing the issue URL. Strictly opt-in per invocation; `--report-dry-run`
//! prints the issue without uploading. The upload goes through the `gh` CLI
//! (its own auth, e.g. `gh auth login` or `GITHUB_TOKEN`); the token is never
//! read, logged, or embedded by hdlsim itself.

use super::bundle::build_bundle_entries;
use super::CommonOpts;
use hdl_lib::bundle::{write_tar, BundleEntry};
use std::path::PathBuf;
use std::process::Command;

/// Default issue repository (override with `HDLSIM_ISSUE_REPO=owner/repo`).
const DEFAULT_REPO: &str = "larsjoost/hdlsim";

pub struct ReportRequest {
    pub message: String,
    pub dry_run: bool,
}

/// Pull the global `--report <msg>` / `--report=<msg>` / `--report-dry-run`
/// flags out of argv (they work before or after the subcommand name).
pub fn extract_report_args(args: &[String]) -> (Vec<String>, Option<ReportRequest>) {
    let mut rest = Vec::new();
    let mut message: Option<String> = None;
    let mut dry_run = false;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--report-dry-run" {
            dry_run = true;
            i += 1;
        } else if a == "--report" {
            match args.get(i + 1) {
                Some(v) if !v.starts_with("--") => {
                    message = Some(v.clone());
                    i += 2;
                }
                _ => {
                    message = Some(String::new());
                    i += 1;
                }
            }
        } else if let Some(v) = a.strip_prefix("--report=") {
            message = Some(v.to_string());
            i += 1;
        } else {
            rest.push(a.clone());
            i += 1;
        }
    }
    let req = message
        .or_else(|| dry_run.then(String::new))
        .map(|m| ReportRequest {
            message: if m.is_empty() {
                "hdlsim command failed".to_string()
            } else {
                m
            },
            dry_run,
        });
    (rest, req)
}

/// Tiny base64 (standard alphabet) encoder — avoids a new dependency.
fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Collect the positional file arguments and -L/--config/-I flags of the
/// failed command, so the reproducer bundle contains the same project view.
fn common_opts_from_args(args: &[String]) -> CommonOpts {
    let mut opts = CommonOpts::default();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "-L" || a == "--lib" {
            if let Some(v) = args.get(i + 1) {
                if let Some((n, p)) = v.split_once('=') {
                    opts.libs.push((n.to_string(), PathBuf::from(p)));
                }
                i += 2;
                continue;
            }
        } else if let Some(v) = a.strip_prefix("--lib=") {
            if let Some((n, p)) = v.split_once('=') {
                opts.libs.push((n.to_string(), PathBuf::from(p)));
            }
        } else if a == "--config" {
            if let Some(v) = args.get(i + 1) {
                opts.config = Some(PathBuf::from(v));
                i += 2;
                continue;
            }
        } else if let Some(v) = a.strip_prefix("--config=") {
            opts.config = Some(PathBuf::from(v));
        } else if a == "-I" || a == "--incdir" {
            if let Some(v) = args.get(i + 1) {
                opts.incdirs.push(PathBuf::from(v));
                i += 2;
                continue;
            }
        } else if let Some(v) = a.strip_prefix("--incdir=") {
            opts.incdirs.push(PathBuf::from(v));
        }
        i += 1;
    }
    opts
}

/// Positional args of the failed command that are existing files.
fn file_args(args: &[String]) -> Vec<PathBuf> {
    if args.is_empty() {
        return Vec::new();
    }
    args[1..]
        .iter()
        .filter(|a| !a.starts_with('-') && std::path::Path::new(a).is_file())
        .map(PathBuf::from)
        .collect()
}

/// Compose the issue title and body. The bundle travels as base64 inside a
/// collapsible section (GitHub attachment uploads need the API; the .tar is
/// small and self-contained, and `tar -x` recovers it).
/// GitHub rejects issue bodies over 65536 chars (GraphQL createIssue); keep a
/// safety margin for the rest of the body.
const BODY_BUDGET: usize = 60_000;

/// Compose the issue title/body. Returns `(title, body, bundle_embedded)` —
/// when the base64 bundle would exceed the body limit, the bundle is left out
/// and `bundle_embedded` is false (the caller saves the tar locally instead).
fn compose_issue(
    message: &str,
    original_args: &[String],
    output: &str,
    tar: &[u8],
    repo: &str,
) -> (String, String, bool) {
    let first = message.lines().next().unwrap_or("").trim();
    let title = format!(
        "[hdlsim] {}",
        if first.len() > 80 {
            format!("{}\u{2026}", &first[..80])
        } else if first.is_empty() {
            "command failed".to_string()
        } else {
            first.to_string()
        }
    );
    // Cap the embedded output so a huge log cannot blow up the issue body.
    const OUTPUT_CAP: usize = 16_000;
    let mut shown = output.trim_end().to_string();
    if shown.len() > OUTPUT_CAP {
        let cut = shown.floor_char_boundary(OUTPUT_CAP);
        shown = format!(
            "{}\n... [truncated {} bytes; full output in repro.tar OUTPUT.txt]",
            &shown[..cut],
            output.len() - cut
        );
    }
    let head = format!(
        "## Problem\n\n{}\n\n## Command\n\n```\nhdlsim {}\n```\n\n\
         ## Output\n\n```\n{}\n```\n\n\
         ## Environment\n\n- hdlsim {}\n- os: {}/{}\n- repo: {}\n\n\
         ## Reproducer bundle\n\n",
        message,
        original_args.join(" "),
        shown,
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        repo
    );
    let embedded = format!(
        "The base64 `.tar` below contains the full project (`src/`, `libs/`, `hdlsim.ini`, \
         `REPRO.md`, `OUTPUT.txt` with the command output above). Decode + extract:\n\n\
         ```\necho '<base64>' | base64 -d > repro.tar && tar -xf repro.tar -C repro && cd repro\n\
         hdlsim run --bundle ../repro.tar --top <top>   # or see REPRO.md\n```\n\n\
         <details><summary>repro.tar (base64, {} bytes)</summary>\n\n```\n{}\n```\n\n</details>\n",
        tar.len(),
        b64(tar)
    );
    let body = format!("{head}{embedded}");
    if body.len() <= BODY_BUDGET {
        return (title, body, true);
    }
    let note = format!(
        "The reproducer bundle ({} bytes) is too large to embed in an issue body \
         (GitHub limit 65536 chars). It was saved as `hdlsim-repro.tar` in the working \
         directory \u{2014} attach it to this issue. To rerun locally: \
         `hdlsim {} --bundle hdlsim-repro.tar`.\n",
        tar.len(),
        original_args
            .first()
            .cloned()
            .unwrap_or_else(|| "check".to_string())
    );
    (title, format!("{head}{note}"), false)
}

/// File the issue (or dry-run it). Called from `main` when a command exited
/// non-zero and `--report` was given. Never touches any token itself — the
/// `gh` CLI does the authenticated API call.
pub fn maybe_file_issue(req: &ReportRequest, original_args: &[String]) {
    let repo = std::env::var("HDLSIM_ISSUE_REPO").unwrap_or_else(|_| DEFAULT_REPO.to_string());
    let run = |msg: String| eprintln!("hdlsim report: {}", msg);

    // Build the reproducer bundle from the same project view as the command.
    let mut opts = common_opts_from_args(original_args);
    if opts.config.is_none() && opts.libs.is_empty() {
        // Mirror the out-of-the-box behavior: index the cwd as `work`.
        opts.libs.push(("work".to_string(), PathBuf::from(".")));
    }
    let files = file_args(original_args);
    let output = hdl_syntax::diagnostic::take_capture();
    let mut entries = match build_bundle_entries(
        &opts,
        None,
        &req.message,
        &files,
        "hdlsim-repro.tar",
    ) {
        Ok(e) => e,
        Err(e) => {
            run(format!(
                "could not build the reproducer bundle ({e}); file manually at https://github.com/{repo}/issues/new"
            ));
            return;
        }
    };
    if !output.is_empty() {
        // Keep the full (untruncated) output inside the bundle.
        let capped = if output.len() > 1_000_000 {
            output[..output.floor_char_boundary(1_000_000)].to_string()
        } else {
            output.clone()
        };
        entries.push(BundleEntry {
            path: "OUTPUT.txt".to_string(),
            data: capped.into_bytes(),
        });
    }
    let tar = match write_tar(&entries) {
        Ok(t) => t,
        Err(e) => {
            run(format!("could not pack the bundle ({e})"));
            return;
        }
    };
    let bundle_path = std::env::temp_dir().join(format!("hdlsim-repro-{}.tar", std::process::id()));
    if std::fs::write(&bundle_path, &tar).is_err() {
        run("could not write the bundle to a temp file".to_string());
        return;
    }
    let (title, body, embedded) = compose_issue(&req.message, original_args, &output, &tar, &repo);
    let local_bundle = if embedded {
        None
    } else {
        let local = std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("hdlsim-repro.tar");
        match std::fs::copy(&bundle_path, &local) {
            Ok(_) => {
                run(format!(
                    "bundle too large to embed in the issue body ({} bytes) \u{2014} saved to {}",
                    tar.len(),
                    local.display()
                ));
                Some(local)
            }
            Err(e) => {
                run(format!(
                    "could not copy bundle to cwd ({e}); it stays at {}",
                    bundle_path.display()
                ));
                None
            }
        }
    };
    let body_path = std::env::temp_dir().join(format!("hdlsim-issue-{}.md", std::process::id()));
    if std::fs::write(&body_path, &body).is_err() {
        run("could not write the issue body to a temp file".to_string());
        return;
    }

    if req.dry_run {
        run(format!(
            "dry run — no issue was created.\n\
             title: {title}\n\
             issue body: {}\n\
             reproducer bundle: {} ({} bytes, {})\n\
             would run: gh issue create --repo {repo} --title <title> --body-file {}",
            body_path.display(),
            bundle_path.display(),
            tar.len(),
            if embedded {
                "embedded in body"
            } else {
                "NOT embedded (too large)"
            },
            body_path.display(),
        ));
        return;
    }

    run(format!(
        "filing a PUBLIC issue at github.com/{repo} — the attached bundle contains your source files"
    ));
    let Some(gh) = which("gh") else {
        run(format!(
            "the `gh` CLI was not found on PATH — install/auth it (`gh auth login`, or set GITHUB_TOKEN), \
             or file manually: https://github.com/{repo}/issues/new (attach {bundle})",
            bundle = bundle_path.display()
        ));
        return;
    };
    let out = Command::new(gh)
        .args([
            "issue",
            "create",
            "--repo",
            &repo,
            "--title",
            &title,
            "--body-file",
            &body_path.to_string_lossy(),
        ])
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let url = String::from_utf8_lossy(&o.stdout).trim().to_string();
            run(format!(
                "issue created: {url} (bundle: {bundle})",
                bundle = bundle_path.display()
            ));
        }
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr);
            let attach = match &local_bundle {
                Some(l) => format!("attach the bundle at {}", l.display()),
                None => format!("attach the bundle at {}", bundle_path.display()),
            };
            run(format!(
                "`gh issue create` failed: {err}\n\
                 file manually: https://github.com/{repo}/issues/new — {attach} \
                 (issue body saved at {body})",
                body = body_path.display()
            ));
        }
        Err(e) => {
            run(format!(
                "could not run `gh` ({e}); file manually: https://github.com/{repo}/issues/new \
                 (bundle: {bundle}, body: {body})",
                bundle = bundle_path.display(),
                body = body_path.display()
            ));
        }
    }
}

fn which(prog: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join(prog))
            .find(|p| p.is_file())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn b64_matches_rfc4648_vectors() {
        assert_eq!(b64(b""), "");
        assert_eq!(b64(b"f"), "Zg==");
        assert_eq!(b64(b"fo"), "Zm8=");
        assert_eq!(b64(b"foo"), "Zm9v");
        assert_eq!(b64(b"foob"), "Zm9vYg==");
        assert_eq!(b64(b"fooba"), "Zm9vYmE=");
        assert_eq!(b64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn extract_report_forms() {
        let (rest, req) = extract_report_args(&[
            "check".to_string(),
            "top.vhd".to_string(),
            "--report".to_string(),
            "it broke".to_string(),
        ]);
        assert_eq!(rest, vec!["check", "top.vhd"]);
        let req = req.unwrap();
        assert_eq!(req.message, "it broke");
        assert!(!req.dry_run);

        let (rest, req) = extract_report_args(&[
            "--report=msg".to_string(),
            "run".to_string(),
            "--report-dry-run".to_string(),
        ]);
        assert_eq!(rest, vec!["run"]);
        let req = req.unwrap();
        assert_eq!(req.message, "msg");
        assert!(req.dry_run);

        let (_, req) = extract_report_args(&["check".to_string()]);
        assert!(req.is_none());

        // `--report-dry-run` alone previews with the default message.
        let (rest, req) = extract_report_args(&[
            "check".to_string(),
            "top.vhd".to_string(),
            "--report-dry-run".to_string(),
        ]);
        assert_eq!(rest, vec!["check", "top.vhd"]);
        let req = req.unwrap();
        assert_eq!(req.message, "hdlsim command failed");
        assert!(req.dry_run);
    }

    #[test]
    fn issue_body_contains_message_command_and_bundle() {
        let (title, body, embedded) = compose_issue(
            "wrong result",
            &["run".to_string(), "tb.v".to_string()],
            "error[E0401]: unknown identifier 'x'",
            b"TAR",
            "o/r",
        );
        assert!(title.contains("wrong result"));
        assert!(embedded);
        assert!(body.contains("wrong result"));
        assert!(body.contains("hdlsim run tb.v"));
        assert!(body.contains("## Output"));
        assert!(body.contains("unknown identifier 'x'"));
        assert!(body.contains("VEFS")); // b64("TAR")
    }

    #[test]
    fn oversized_bundle_is_not_embedded() {
        // A bundle whose base64 form would exceed the GitHub body limit must
        // be left out of the body (and reported as not embedded).
        let big = vec![b'x'; 60_000];
        let (title, body, embedded) = compose_issue(
            "big project fails",
            &["check".to_string(), "a.vhd".to_string()],
            "error: boom",
            &big,
            "o/r",
        );
        assert!(!embedded);
        assert!(title.contains("big project fails"));
        assert!(body.contains("too large to embed"));
        assert!(body.contains("hdlsim-repro.tar"));
        assert!(!body.contains("<details>"));
        assert!(body.len() <= BODY_BUDGET + 2_000);
    }
}
