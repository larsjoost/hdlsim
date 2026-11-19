//! `--report <msg>` — when a command fails, file a GitHub issue with the
//! reproducer bundle uploaded as a gist and linked from the issue body.
//! Strictly opt-in per invocation; `--report-dry-run` prints the issue without
//! uploading. The upload goes through the `gh` CLI (its own auth, e.g.
//! `gh auth login` or `GITHUB_TOKEN`); the token is never read, logged, or
//! embedded by hdlsim itself.

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

/// Diagnostic codes (`[E0401]`, `[W0401]`, ...) appearing in the captured
/// output, in first-seen order, deduplicated.
fn diagnostic_codes(output: &str) -> Vec<String> {
    let mut codes: Vec<String> = Vec::new();
    let b = output.as_bytes();
    let mut i = 0;
    while i + 7 <= b.len() {
        if b[i] == b'['
            && (b[i + 1] == b'E' || b[i + 1] == b'W')
            && b[i + 2..i + 6].iter().all(|c| c.is_ascii_digit())
            && b[i + 6] == b']'
        {
            let code = output[i + 1..i + 6].to_string();
            if !codes.contains(&code) {
                codes.push(code);
            }
            i += 7;
        } else {
            i += 1;
        }
    }
    codes
}

/// Compose the issue title/body. The bundle itself is never embedded in the
/// body (GitHub rejects bodies over 65536 chars, and base64 inflates the tar);
/// it is linked from an uploaded gist, or saved locally as a fallback.
fn compose_issue(
    message: &str,
    original_args: &[String],
    output: &str,
    exit_code: i32,
    tar_len: usize,
    repo: &str,
    gist: Option<&str>,
) -> (String, String, BundleRef) {
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
            "{}\n... [truncated {} bytes; full output in the bundle's OUTPUT.txt]",
            &shown[..cut],
            output.len() - cut
        );
    }
    let note = match gist {
        Some(url) => format!(
            "The reproducer bundle ({tar_len} bytes) was uploaded as a gist: {url}\n\
             Raw bundle (base64 tar): `{url}/raw/hdlsim-repro.tar.b64` — decode with \
             `... | base64 -d > repro.tar`, then rerun per REPRO.md inside.\n"
        ),
        None => format!(
            "The reproducer bundle ({tar_len} bytes) was saved as `hdlsim-repro.tar` in the \
             working directory — attach it to this issue.\n"
        ),
    };
    let codes_section = {
        let codes = diagnostic_codes(output);
        if codes.is_empty() {
            String::new()
        } else {
            let mut s = String::from(
                "## Diagnostic codes\n\n\
                 Scanned from the output above (`hdlsim explain <code>` prints the same text):\n\n",
            );
            for c in &codes {
                match hdl_syntax::diagnostic::explain_code(c) {
                    Some(ex) => s.push_str(&format!("- `{c}`: {ex}\n")),
                    None => s.push_str(&format!("- `{c}`\n")),
                }
            }
            s.push_str("\n");
            s
        }
    };
    let body = format!(
        "## Problem\n\n{message}\n\n## Command\n\n```\nhdlsim {}\n```\n\n\
         ## Output\n\n```\n{shown}\n```\n\n\
         {codes_section}\
         ## Environment\n\n- hdlsim {}\n- os: {}/{}\n- repo: {repo}\n- exit code: {exit_code}\n\n\
         ## Reproducer bundle\n\n{note}\n\
         ## Reproduce from the bundle\n\n\
         From the bundle above (gist raw link, or the attached/local `hdlsim-repro.tar`):\n\n\
         ```\nhdlsim {} --bundle hdlsim-repro.tar\n```\n",
        original_args.join(" "),
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        original_args.join(" "),
    );
    let bref = match gist {
        Some(url) => BundleRef::Gist(url.to_string()),
        None => BundleRef::Local,
    };
    (title, body, bref)
}

/// How the reproducer bundle is referenced from the issue.
enum BundleRef {
    Gist(String),
    Local,
}

/// File the issue (or dry-run it). Called from `main` when a command exited
/// non-zero and `--report` was given; `exit_code` is the command's exit code.
/// Never touches any token itself — the `gh` CLI does the authenticated API
/// call.
pub fn maybe_file_issue(req: &ReportRequest, original_args: &[String], exit_code: i32) {
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
    // The bundle always goes to a gist (linked from the issue body); on
    // dry-run or gist failure it is saved locally instead.
    let gist = if req.dry_run {
        None
    } else {
        match upload_gist(&tar, &req.message) {
            Ok(url) => Some(url),
            Err(e) => {
                run(format!(
                    "could not upload the reproducer bundle as a gist ({e}); \
                     falling back to a local bundle — attach it to the issue manually"
                ));
                None
            }
        }
    };
    let (title, body, bref, local_bundle) = match gist {
        Some(url) => {
            run(format!(
                "reproducer bundle ({} bytes) uploaded to gist {url}",
                tar.len()
            ));
            let (t, b, r) = compose_issue(
                &req.message,
                original_args,
                &output,
                exit_code,
                tar.len(),
                &repo,
                Some(&url),
            );
            (t, b, r, None)
        }
        None => {
            let (t, b, r) = compose_issue(
                &req.message,
                original_args,
                &output,
                exit_code,
                tar.len(),
                &repo,
                None,
            );
            let local = std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join("hdlsim-repro.tar");
            let copied = std::fs::copy(&bundle_path, &local)
                .ok()
                .map(|_| local.clone());
            if let Some(l) = &copied {
                run(format!(
                    "could not upload a gist \u{2014} bundle ({} bytes) saved to {}",
                    tar.len(),
                    l.display()
                ));
            }
            (t, b, r, copied)
        }
    };
    let body_path = std::env::temp_dir().join(format!("hdlsim-issue-{}.md", std::process::id()));
    if std::fs::write(&body_path, &body).is_err() {
        run("could not write the issue body to a temp file".to_string());
        return;
    }

    if req.dry_run {
        run(format!(
            "dry run — no issue or gist was created.\n\
             title: {title}\n\
             issue body: {}\n\
             reproducer bundle: {} ({} bytes, {})\n\
             would run: gh issue create --repo {repo} --title <title> --body-file {}",
            body_path.display(),
            bundle_path.display(),
            tar.len(),
            match &bref {
                BundleRef::Gist(_) => "would be uploaded as a gist and linked",
                BundleRef::Local => {
                    "would be uploaded as a gist and linked (dry-run does not upload; fallback ./hdlsim-repro.tar)"
                }
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
            let attach = match &bref {
                BundleRef::Gist(url) => {
                    format!("the bundle is at {url} (linked in the body draft)")
                }
                _ => match &local_bundle {
                    Some(l) => format!("attach the bundle at {}", l.display()),
                    None => format!("attach the bundle at {}", bundle_path.display()),
                },
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

/// Where the base64 bundle is staged before `gh gist create`. The file name
/// matters: `gh gist create <file>` names the gist file after the local
/// basename, and the issue body links `{gist}/raw/hdlsim-repro.tar.b64`, so
/// the staged name must be exactly `hdlsim-repro.tar.b64`.
fn gist_staging_path() -> PathBuf {
    std::env::temp_dir()
        .join(format!("hdlsim-gist-{}", std::process::id()))
        .join("hdlsim-repro.tar.b64")
}

/// The visibility flag for `gh gist create`: secret gists are the default
/// and gh has NO `--secret` flag (passing it fails with "unknown flag:
/// --secret", which used to make every upload fail), so `--public` is only
/// passed when explicitly requested.
fn gist_public_arg(public: bool) -> Option<&'static str> {
    public.then_some("--public")
}

/// Upload the bundle (base64) as a gist via the `gh` CLI; `Ok(url)` on success,
/// `Err(reason)` with gh's stderr on failure. Secret (unlisted) by default;
/// set HDLSIM_REPORT_GIST=public for a public gist. Gist files are
/// size-limited, so the base64 form is capped at 8 MB.
fn upload_gist(tar: &[u8], title: &str) -> Result<String, String> {
    const GIST_B64_CAP: usize = 8 * 1024 * 1024;
    let gh = which("gh").ok_or_else(|| "the `gh` CLI was not found on PATH".to_string())?;
    let b64tar = b64(tar);
    if b64tar.len() > GIST_B64_CAP {
        return Err(format!(
            "bundle too large for a gist ({} bytes base64, cap {GIST_B64_CAP})",
            b64tar.len()
        ));
    }
    let path = gist_staging_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    std::fs::write(&path, &b64tar).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    let public = matches!(
        std::env::var("HDLSIM_REPORT_GIST").as_deref(),
        Ok("public") | Ok("1")
    );
    let mut cmd = Command::new(gh);
    cmd.arg("gist")
        .arg("create")
        .arg("--desc")
        .arg(format!("hdlsim reproducer bundle — {title}"));
    if let Some(flag) = gist_public_arg(public) {
        cmd.arg(flag);
    }
    cmd.arg(&path);
    let out = cmd.output().map_err(|e| format!("cannot run `gh`: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "`gh gist create` failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    // Accept github.com and GitHub Enterprise gist hosts (gist.<host>/...).
    if url.starts_with("https://gist.") {
        Ok(url)
    } else {
        Err(format!("unexpected `gh gist create` output: {url}"))
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
    fn gist_upload_uses_valid_gh_flags_and_staging_name() {
        // `gh gist create` has no `--secret` flag (secret is the default) —
        // passing it made every upload fail with "unknown flag: --secret", so
        // issues were filed with no bundle attached (issue #113).
        assert_eq!(gist_public_arg(false), None);
        assert_eq!(gist_public_arg(true), Some("--public"));
        let p = gist_staging_path();
        // The gist file takes the local basename; the issue body links
        // `{gist}/raw/hdlsim-repro.tar.b64`, so the staged name must match.
        assert_eq!(p.file_name().unwrap(), "hdlsim-repro.tar.b64");
    }

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
    fn diagnostic_codes_scans_and_dedupes() {
        assert_eq!(diagnostic_codes(""), Vec::<String>::new());
        let out = "warning[W0401]: a\nerror[E0401]: b\n\u{1b}[1;33mwarning[W0401]:^\u{1b}[0m c\nerror[E0301]: d\nnot a [E12] code\n[x0000]";
        assert_eq!(diagnostic_codes(out), vec!["W0401", "E0401", "E0301"]);
    }

    #[test]
    fn issue_body_contains_message_command_output_and_gist_link() {
        let (title, body, bref) = compose_issue(
            "wrong result",
            &["run".to_string(), "tb.v".to_string()],
            "error[E0401]: unknown identifier 'x'\nwarning[W0401]: degraded",
            1,
            4096,
            "o/r",
            Some("https://gist.github.com/abc123"),
        );
        assert!(title.contains("wrong result"));
        assert!(matches!(bref, BundleRef::Gist(ref u) if u == "https://gist.github.com/abc123"));
        assert!(body.contains("wrong result"));
        assert!(body.contains("hdlsim run tb.v"));
        assert!(body.contains("## Output"));
        assert!(body.contains("unknown identifier 'x'"));
        assert!(body.contains("https://gist.github.com/abc123/raw/hdlsim-repro.tar.b64"));
        assert!(body.contains("4096 bytes"));
        assert!(body.contains("exit code: 1"));
        assert!(body.contains("## Diagnostic codes"));
        assert!(body.contains("`E0401`:"));
        assert!(body.contains("`W0401`:"));
        assert!(body.contains("## Reproduce from the bundle"));
        assert!(body.contains("hdlsim run tb.v --bundle hdlsim-repro.tar"));
    }

    #[test]
    fn issue_body_without_gist_points_at_local_bundle() {
        let (title, body, bref) = compose_issue(
            "big project fails",
            &["check".to_string(), "a.vhd".to_string()],
            "error: boom",
            1,
            60_000,
            "o/r",
            None,
        );
        assert!(matches!(bref, BundleRef::Local));
        assert!(title.contains("big project fails"));
        assert!(body.contains("hdlsim-repro.tar"));
        assert!(body.contains("60000 bytes"));
        assert!(body.contains("hdlsim check a.vhd --bundle hdlsim-repro.tar"));
        assert!(!body.contains("gist.github.com"));
        // No [E....]/[W....] code in the output: no Diagnostic codes section.
        assert!(!body.contains("## Diagnostic codes"));
    }

    #[test]
    fn oversized_output_is_truncated_in_body() {
        let big_out = "x".repeat(100_000);
        let (_, body, _) = compose_issue(
            "m",
            &["check".to_string()],
            &big_out,
            2,
            10,
            "o/r",
            Some("https://gist.github.com/z"),
        );
        assert!(body.contains("truncated"));
        assert!(body.contains("exit code: 2"));
        assert!(body.len() < 30_000);
    }
}
