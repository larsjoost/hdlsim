//! `hdlsim assist` — local, deterministic prompt-to-task router — and
//! `hdlsim explain <code>` — human-readable explanation of a diagnostic or
//! lint code.
//!
//! `assist` is deliberately rule-based (keyword routing), not a cloud LLM:
//! hdlsim is offline and zero-dependency, and a prompt never leaves the
//! machine. It maps a free-text prompt to the hdlsim subcommand that performs
//! the task, prints the exact command line (`$ hdlsim ...` plus a rationale),
//! and with `--yes` executes it through the normal `dispatch` path. Two rules
//! answer inline instead of suggesting: a prompt containing a diagnostic code
//! (`E0301`, `W0102`, ...) prints its explanation, and a check/compile prompt
//! runs the check in-process and annotates every diagnostic with its
//! explanation.

use super::{
    build_manager, dispatch, parse_with, remap_into_bundle, Command, CommonArgs, CommonOpts,
};
use clap::Parser;
use hdl_ir::elab;
use hdl_ir::Design;
use hdl_syntax::diagnostic::{explain_code, explained_codes};
use std::path::{Path, PathBuf};

/// One routed task: the command line to run, why it was chosen, and whether
/// `assist` already produced the output itself (`inline`), in which case
/// `--yes` has nothing left to execute.
pub struct Suggestion {
    pub command: Vec<String>,
    pub rationale: String,
    pub inline: bool,
}

/// Lowercase words of a prompt (`-` kept, so `do-file` stays one word).
fn words(prompt: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in prompt.chars() {
        if ch.is_alphanumeric() || ch == '-' {
            cur.push(ch.to_ascii_lowercase());
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// First diagnostic/lint code (`E0301`, `W0102`, ...) in the prompt, with word
/// boundaries on both sides so `xE0301` or `E03012` do not match.
fn extract_code(prompt: &str) -> Option<String> {
    let b = prompt.as_bytes();
    for i in 0..b.len() {
        if (b[i] == b'E' || b[i] == b'W')
            && [1usize, 2, 3, 4]
                .iter()
                .all(|k| b.get(i + k).is_some_and(|c| c.is_ascii_digit()))
            && (i == 0 || !b[i - 1].is_ascii_alphanumeric())
            && !b.get(i + 5).is_some_and(|c| c.is_ascii_digit())
        {
            return Some(prompt[i..i + 5].to_string());
        }
    }
    None
}

/// Whitespace-separated tokens that look like HDL source files.
fn extract_files(prompt: &str) -> Vec<String> {
    prompt
        .split_whitespace()
        .map(|tok| {
            tok.trim_matches(|c: char| {
                matches!(
                    c,
                    ',' | ';' | ':' | '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\'' | '`'
                )
            })
        })
        .filter(|t| {
            let lower = t.to_ascii_lowercase();
            [".v", ".sv", ".vh", ".vhd", ".vhdl"]
                .iter()
                .any(|e| lower.ends_with(e))
        })
        .map(|t| t.to_string())
        .collect()
}

fn hits(ws: &[String], kws: &[&str]) -> bool {
    kws.iter().any(|k| ws.iter().any(|w| w == k))
}

/// Route a prompt to a task. `top` is the `--top` unit if given; `files` are
/// the file-like tokens extracted from the prompt. Pure: no I/O, so the
/// routing table is unit-testable.
pub fn route(prompt: &str, top: Option<&str>, files: &[String]) -> Suggestion {
    let ws = words(prompt);
    let top_or = top.unwrap_or("<top>");
    // 1. A diagnostic code in the prompt: explain it inline.
    if let Some(code) = extract_code(prompt) {
        return Suggestion {
            command: vec!["explain".to_string(), code.clone()],
            rationale: format!("the prompt mentions diagnostic code {}", code),
            inline: true,
        };
    }
    // 2. Run the testbenches as a regression with a JUnit report.
    if hits(
        &ws,
        &[
            "test",
            "tests",
            "testbench",
            "testbenches",
            "regress",
            "regression",
            "junit",
        ],
    ) {
        let mut command = vec![
            "regress".to_string(),
            "--junit".to_string(),
            "results.xml".to_string(),
        ];
        if let Some(t) = top {
            command.push(t.to_string());
        }
        return Suggestion {
            command,
            rationale: "run the testbenches and write a JUnit XML report".to_string(),
            inline: false,
        };
    }
    // 3. Lint, applying the safe automatic fixes.
    if hits(&ws, &["lint", "linter"]) {
        let mut command = vec!["lint".to_string(), "--fix".to_string()];
        command.extend(files.iter().cloned());
        return Suggestion {
            command,
            rationale: "lint the mentioned files and apply safe fixes".to_string(),
            inline: false,
        };
    }
    // 4. Format the mentioned files in place.
    if hits(
        &ws,
        &[
            "format",
            "formatting",
            "fmt",
            "pretty",
            "prettify",
            "beautify",
        ],
    ) {
        let mut command = vec!["fmt".to_string(), "-w".to_string()];
        command.extend(files.iter().cloned());
        return Suggestion {
            command,
            rationale: "format the mentioned files in place".to_string(),
            inline: false,
        };
    }
    // 5. Check/compile (e.g. "why does tb.v fail"): run check in-process on
    //    the mentioned files and explain every diagnostic inline.
    if hits(
        &ws,
        &[
            "check",
            "compile",
            "compilation",
            "error",
            "errors",
            "fail",
            "fails",
            "failing",
            "broken",
            "why",
            "diagnose",
            "diagnostics",
        ],
    ) {
        let mut command = vec!["check".to_string()];
        command.extend(files.iter().cloned());
        let inline = !files.is_empty();
        return Suggestion {
            command,
            rationale: "compile the mentioned files and report diagnostics (explained inline)"
                .to_string(),
            inline,
        };
    }
    // 6. Dependency listing.
    if hits(
        &ws,
        &[
            "depend",
            "depends",
            "dependency",
            "dependencies",
            "deps",
            "files",
            "import",
            "imports",
            "include",
            "includes",
        ],
    ) {
        let mut command = vec!["deps".to_string(), "--top".to_string(), top_or.to_string()];
        command.extend(files.iter().cloned());
        return Suggestion {
            command,
            rationale: "list every file the top unit depends on, transitively".to_string(),
            inline: false,
        };
    }
    // 7. Waveform dump.
    if hits(
        &ws,
        &[
            "wave",
            "waves",
            "waveform",
            "waveforms",
            "vcd",
            "trace",
            "dump",
            "gtkwave",
        ],
    ) {
        let mut command = vec![
            "run".to_string(),
            "--top".to_string(),
            top_or.to_string(),
            "--vcd".to_string(),
            "waves.vcd".to_string(),
        ];
        command.extend(files.iter().cloned());
        return Suggestion {
            command,
            rationale: "simulate the top and dump a VCD waveform".to_string(),
            inline: false,
        };
    }
    // 8. Run a do-file script.
    if hits(
        &ws,
        &["script", "scripts", "tcl", "do-file", "dofile", "macro"],
    ) {
        let mut command = vec![
            "do".to_string(),
            "script.do".to_string(),
            "--top".to_string(),
            top_or.to_string(),
        ];
        command.extend(files.iter().cloned());
        return Suggestion {
            command,
            rationale: "run a mini-Tcl do-file against the top".to_string(),
            inline: false,
        };
    }
    // 9. Editor (LSP) integration.
    if hits(
        &ws,
        &[
            "lsp",
            "editor",
            "editors",
            "vscode",
            "hover",
            "definition",
            "completion",
            "autocomplete",
        ],
    ) {
        return Suggestion {
            command: vec!["lsp".to_string()],
            rationale: "start the language server over stdio for editor integration".to_string(),
            inline: false,
        };
    }
    // 10. Help or unmatched: print the task list inline.
    let known = hits(&ws, &["help", "tasks", "overview"]) || prompt.contains("what can");
    Suggestion {
        command: vec!["--help".to_string()],
        rationale: if known {
            "list the tasks assist can route to".to_string()
        } else {
            "no rule matched this prompt; showing the task list".to_string()
        },
        inline: true,
    }
}

/// The task list printed for help/fallback prompts.
fn task_list() -> String {
    let mut out = String::from("hdlsim assist routes prompts to tasks (offline, rule-based):\n");
    for line in [
        "  \"why does tb.v fail\" / check / compile -> hdlsim check tb.v (diagnostics explained inline)",
        "  \"run the tests\" / regress / junit     -> hdlsim regress --junit results.xml <top>",
        "  \"lint\"                                -> hdlsim lint --fix <files>",
        "  \"format / pretty\"                     -> hdlsim fmt -w <files>",
        "  \"what files does tb depend on\"        -> hdlsim deps --top tb <files>",
        "  \"show the waveform / vcd / trace\"     -> hdlsim run --top tb --vcd waves.vcd <files>",
        "  \"run a script / tcl\"                  -> hdlsim do script.do --top tb <files>",
        "  \"editor / lsp / hover\"                -> hdlsim lsp",
        "  \"E0301\" / \"W0102\" ...                  -> hdlsim explain <code>",
        "add --yes to execute the suggested command",
    ] {
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Rebuild the common flag tokens (`-L`, `--config`, `-I`, `--bundle`) so a
/// suggested command can be re-dispatched with the same library setup.
fn common_args(opts: &CommonOpts) -> Vec<String> {
    let mut out = Vec::new();
    for (n, p) in &opts.libs {
        out.push("-L".to_string());
        out.push(format!("{}={}", n, p.display()));
    }
    if let Some(c) = &opts.config {
        out.push("--config".to_string());
        out.push(c.display().to_string());
    }
    for d in &opts.incdirs {
        out.push("-I".to_string());
        out.push(d.display().to_string());
    }
    if let Some(b) = &opts.bundle {
        out.push("--bundle".to_string());
        out.push(b.display().to_string());
    }
    out
}

/// Compile + elaborate the given files and print every diagnostic annotated
/// with its code explanation (the inline answer for check/compile prompts).
fn run_check_inline(opts: &CommonOpts, files: &[String]) -> anyhow::Result<()> {
    let (mut mgr, root) = build_manager(opts).map_err(anyhow::Error::msg)?;
    let paths: Vec<PathBuf> = files
        .iter()
        .map(|f| remap_into_bundle(Path::new(f), &root))
        .collect();
    let mut design = Design::default();
    mgr.compile_files(&mut design, &paths);
    let tops: Vec<String> = design.modules.iter().map(|m| m.name.clone()).collect();
    for t in &tops {
        let _ = elab::elaborate_with(&mut design, t, &mut mgr);
    }
    let diags = mgr.diags();
    if diags.is_empty() {
        println!("no diagnostics: {} file(s) compile clean", paths.len());
        return Ok(());
    }
    for d in &diags.items {
        eprintln!("{}", d.render(mgr.db()));
        if let Some(ex) = explain_code(d.code) {
            eprintln!("   = note[{}]: {}", d.code, ex);
        }
        eprintln!();
    }
    Ok(())
}

fn print_code_explanation(code: &str) {
    match explain_code(code) {
        Some(text) => println!("{}: {}", code, text),
        None => println!("{}: no explanation known for this code", code),
    }
}

pub struct Assist;

#[derive(Parser)]
#[command(
    about = "describe what you want in plain English, get the command: hdlsim assist \"why does tb.v fail\""
)]
struct AssistArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// Execute the suggested command instead of only printing it.
    #[arg(long, short = 'y')]
    yes: bool,
    /// Top-level unit for context.
    #[arg(long)]
    top: Option<String>,
    /// Prompt words.
    prompt: Vec<String>,
}

impl Command for Assist {
    fn name(&self) -> &'static str {
        "assist"
    }
    fn about(&self) -> &'static str {
        "describe what you want in plain English, get the command: hdlsim assist \"why does tb.v fail\""
    }
    fn run(&self, args: &[String]) -> anyhow::Result<i32> {
        let a = match parse_with::<AssistArgs>("assist", args) {
            Ok(a) => a,
            Err(code) => return Ok(code),
        };
        let opts = a.common.into_opts()?;
        let yes = a.yes;
        let top = a.top;
        let prompt = a.prompt.join(" ");
        if prompt.is_empty() {
            return Err(anyhow::anyhow!(
                "usage: hdlsim assist [--yes] [--top T] \"<prompt>\""
            ));
        }
        let files = extract_files(&prompt);
        let s = route(&prompt, top.as_deref(), &files);
        println!("hdlsim assist: {}", s.rationale);
        let mut line = String::from("$ hdlsim");
        for a in &s.command {
            if a.chars().any(|c| c.is_whitespace()) {
                line.push_str(&format!(" \"{}\"", a));
            } else {
                line.push_str(&format!(" {}", a));
            }
        }
        println!("{}", line);
        if s.inline {
            if let Some(code) = extract_code(&prompt) {
                print_code_explanation(&code);
            } else if s.command.first().is_some_and(|c| c == "check") {
                run_check_inline(&opts, &files).map_err(anyhow::Error::msg)?;
            } else {
                print!("{}", task_list());
            }
        }
        if yes && !s.inline {
            let mut argv = common_args(&opts);
            argv.extend(s.command.iter().cloned());
            return dispatch(&argv);
        }
        Ok(0)
    }
}

pub struct Explain;

#[derive(Parser)]
#[command(about = "explain an error or warning code and how to fix it: hdlsim explain E0301")]
struct ExplainArgs {
    /// Diagnostic/lint code (e.g. E0301); omit to list all codes.
    code: Option<String>,
}

impl Command for Explain {
    fn name(&self) -> &'static str {
        "explain"
    }
    fn about(&self) -> &'static str {
        "explain an error or warning code and how to fix it: hdlsim explain E0301"
    }
    fn run(&self, args: &[String]) -> anyhow::Result<i32> {
        let a = match parse_with::<ExplainArgs>("explain", args) {
            Ok(a) => a,
            Err(code) => return Ok(code),
        };
        match a.code.as_deref() {
            None => {
                for (code, text) in explained_codes() {
                    println!("{}: {}", code, text);
                }
                Ok(0)
            }
            Some(code) => match explain_code(code) {
                Some(text) => {
                    println!("{}: {}", code, text);
                    Ok(0)
                }
                None => {
                    eprintln!("hdlsim explain: unknown code '{}'", code);
                    eprintln!("known codes:");
                    for (c, t) in explained_codes() {
                        eprintln!("  {}: {}", c, t);
                    }
                    Ok(1)
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_codes_with_boundaries() {
        assert_eq!(extract_code("why E0301 here"), Some("E0301".to_string()));
        assert_eq!(
            extract_code("warning W0102 in always"),
            Some("W0102".to_string())
        );
        assert_eq!(extract_code("no code"), None);
        assert_eq!(extract_code("xE0301"), None);
        assert_eq!(extract_code("E03012"), None);
    }

    #[test]
    fn extracts_file_tokens() {
        assert_eq!(
            extract_files("check rtl/counter.v and tb.vhd, plus a.sv"),
            vec!["rtl/counter.v", "tb.vhd", "a.sv"]
        );
        assert!(extract_files("no files here").is_empty());
    }

    #[test]
    fn routes_code_prompt_to_explain() {
        let s = route("what does E0301 mean", None, &[]);
        assert_eq!(s.command, vec!["explain", "E0301"]);
        assert!(s.inline);
    }

    #[test]
    fn routes_task_keywords() {
        let s = route("run the tests", Some("tb"), &[]);
        assert_eq!(s.command, vec!["regress", "--junit", "results.xml", "tb"]);
        assert!(!s.inline);
        assert_eq!(
            route("please lint counter.v", None, &["counter.v".to_string()]).command,
            vec!["lint", "--fix", "counter.v"]
        );
        assert_eq!(
            route("format my code", None, &["a.v".to_string()]).command,
            vec!["fmt", "-w", "a.v"]
        );
        let s = route("why does tb.v fail", None, &["tb.v".to_string()]);
        assert_eq!(s.command, vec!["check", "tb.v"]);
        assert!(s.inline);
        assert_eq!(
            route("what files does tb depend on", Some("tb"), &[]).command,
            vec!["deps", "--top", "tb"]
        );
        assert_eq!(
            route("show me the vcd trace", Some("tb"), &["tb.v".to_string()]).command,
            vec!["run", "--top", "tb", "--vcd", "waves.vcd", "tb.v"]
        );
        assert_eq!(
            route("run a tcl script", Some("tb"), &[]).command,
            vec!["do", "script.do", "--top", "tb"]
        );
        assert_eq!(route("editor integration", None, &[]).command, vec!["lsp"]);
        let s = route("frobnicate the widget", None, &[]);
        assert_eq!(s.command, vec!["--help"]);
        assert!(s.inline);
    }
}
