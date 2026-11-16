//! `hdlsim regress` — run a list of testbenches, print a summary, and emit a
//! JUnit XML report (phase 2 of docs/plan.md). Each test is one `run`
//! (fresh design + fresh manager per test, so the on-disk parse cache is
//! shared but designs never leak between tests). Pass/fail comes from the
//! existing `TestFailed`/exit-code path; `--coverage` attaches a
//! CoverageCollector per test and folds its report into the XML.

use super::{
    build_manager, parse_time, parse_with, remap_into_bundle, Command, CommonArgs, CommonOpts,
};
use clap::Parser;
use hdl_ir::elab;
use hdl_ir::Design;
use hdl_sim::coverage::{CoverageCollector, SharedCollector};
use hdl_sim::{Observer, SimEvent, Simulator};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

/// Captures transcript + failures for one test.
struct Capture {
    failures: Vec<String>,
}

impl Observer for Capture {
    fn on_event(&mut self, ev: &SimEvent) {
        if let SimEvent::TestFailed { text, .. } = ev {
            self.failures.push(text.clone());
        }
    }
}

struct TestSpec {
    name: String,
    top: String,
    files: Vec<PathBuf>,
}

struct TestResult {
    name: String,
    passed: bool,
    secs: f64,
    detail: String,
    coverage: Option<String>,
}

#[derive(Parser)]
#[command(
    about = "run a regression list of testbenches: hdlsim regress [--list tests.txt] [--junit out.xml] [--max-time T] [--coverage] [tops...]"
)]
struct RegressArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// Write a JUnit XML report to this file.
    #[arg(long)]
    junit: Option<PathBuf>,
    /// Test list file (one `top` or `name=top` per line).
    #[arg(long)]
    list: Option<PathBuf>,
    /// Per-test stop time (e.g. 10us).
    #[arg(long)]
    max_time: Option<String>,
    /// Attach a coverage collector per test and fold it into the report.
    #[arg(long)]
    coverage: bool,
    /// Test tops.
    tops: Vec<String>,
}

pub struct Regress;

impl Command for Regress {
    fn name(&self) -> &'static str {
        "regress"
    }

    fn about(&self) -> &'static str {
        "run testbenches, emit JUnit XML: hdlsim regress [--junit out.xml] [--list tests.txt] [--max-time 10us] [--coverage] [top ...]"
    }

    fn run(&self, args: &[String]) -> Result<i32, String> {
        let a = match parse_with::<RegressArgs>("regress", args) {
            Ok(a) => a,
            Err(code) => return Ok(code),
        };
        let opts = a.common.into_opts()?;
        let junit = a.junit;
        let list = a.list;
        let max_time_fs = match &a.max_time {
            Some(v) => parse_time(v)?,
            None => u64::MAX,
        };
        let coverage = a.coverage;
        let tops = a.tops;
        let mut tests: Vec<TestSpec> = Vec::new();
        if let Some(lf) = &list {
            let text = std::fs::read_to_string(lf)
                .map_err(|e| format!("cannot read {}: {}", lf.display(), e))?;
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let mut toks = line.split_whitespace();
                let first = toks.next().unwrap();
                // `name=top` sets a distinct test name; otherwise name == top.
                let (name, top) = match first.split_once('=') {
                    Some((n, t)) => (n.to_string(), t.to_string()),
                    None => (first.to_string(), first.to_string()),
                };
                let files = toks.map(PathBuf::from).collect();
                tests.push(TestSpec { name, top, files });
            }
        }
        for t in tops {
            tests.push(TestSpec {
                name: t.clone(),
                top: t,
                files: Vec::new(),
            });
        }
        if tests.is_empty() {
            return Err("no tests: pass top names and/or --list FILE".to_string());
        }

        let mut results = Vec::new();
        for spec in &tests {
            println!("hdlsim: running {} ...", spec.name);
            let r = run_one(&opts, spec, max_time_fs, coverage);
            println!(
                "hdlsim: {} {} ({:.3}s){}",
                spec.name,
                if r.passed { "PASS" } else { "FAIL" },
                r.secs,
                if r.detail.is_empty() {
                    String::new()
                } else {
                    format!(" — {}", r.detail)
                }
            );
            results.push(r);
        }
        let failures = results.iter().filter(|r| !r.passed).count();
        let total_secs: f64 = results.iter().map(|r| r.secs).sum();
        println!(
            "hdlsim: regress: {}/{} passed, {} failed ({:.3}s total)",
            results.len() - failures,
            results.len(),
            failures,
            total_secs
        );
        if let Some(path) = &junit {
            let xml = junit_xml(&results, total_secs);
            std::fs::write(path, xml)
                .map_err(|e| format!("cannot write {}: {}", path.display(), e))?;
            println!("hdlsim: JUnit XML written to {}", path.display());
        }
        Ok(if failures > 0 { 1 } else { 0 })
    }
}

fn run_one(opts: &CommonOpts, spec: &TestSpec, max_time_fs: u64, coverage: bool) -> TestResult {
    let t0 = Instant::now();
    let fail = |detail: String| TestResult {
        name: spec.name.clone(),
        passed: false,
        secs: t0.elapsed().as_secs_f64(),
        detail,
        coverage: None,
    };
    let Ok((mut mgr, root)) = build_manager(opts) else {
        return fail("cannot build library manager".to_string());
    };
    let files: Vec<PathBuf> = spec
        .files
        .iter()
        .map(|f| remap_into_bundle(f, &root))
        .collect();
    let mut design = Design::default();
    mgr.compile_files(&mut design, &files);
    if mgr.diags().has_errors() {
        return fail(
            mgr.diags()
                .render_all(mgr.db())
                .lines()
                .next()
                .unwrap_or("")
                .to_string(),
        );
    }
    let flat = match elab::elaborate_with(&mut design, &spec.top, &mut mgr) {
        Ok(f) => f,
        Err(e) => return fail(format!("elaboration error: {}", e)),
    };
    if mgr.diags().has_errors() {
        return fail(
            mgr.diags()
                .render_all(mgr.db())
                .lines()
                .next()
                .unwrap_or("")
                .to_string(),
        );
    }
    let mut sim = Simulator::new(flat);
    let cap = Rc::new(RefCell::new(Capture {
        failures: Vec::new(),
    }));
    sim.add_observer(Box::new(CapForward(cap.clone())));
    let cov = if coverage {
        let widths: Vec<usize> = sim.flat().signals.iter().map(|s| s.width).collect();
        let c = Rc::new(RefCell::new(CoverageCollector::new(&design, &widths)));
        sim.add_observer(Box::new(SharedCollector(c.clone())));
        Some(c)
    } else {
        None
    };
    sim.run(max_time_fs);
    let cov_report = cov.map(|c| c.borrow().report(mgr.db()));
    let failures = cap.borrow().failures.clone();
    let passed = sim.failures() == 0 && failures.is_empty();
    TestResult {
        name: spec.name.clone(),
        passed,
        secs: t0.elapsed().as_secs_f64(),
        detail: failures.first().cloned().unwrap_or_default(),
        coverage: cov_report,
    }
}

struct CapForward(Rc<RefCell<Capture>>);
impl Observer for CapForward {
    fn on_event(&mut self, ev: &SimEvent) {
        self.0.borrow_mut().on_event(ev);
    }
}

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

fn junit_xml(results: &[TestResult], total_secs: f64) -> String {
    let failures = results.iter().filter(|r| !r.passed).count();
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str(&format!(
        "<testsuites tests=\"{}\" failures=\"{}\" time=\"{:.3}\">\n",
        results.len(),
        failures,
        total_secs
    ));
    out.push_str(&format!(
        "  <testsuite name=\"hdlsim\" tests=\"{}\" failures=\"{}\" time=\"{:.3}\">\n",
        results.len(),
        failures,
        total_secs
    ));
    for r in results {
        if r.passed {
            out.push_str(&format!(
                "    <testcase name=\"{}\" classname=\"hdlsim\" time=\"{:.3}\"/>\n",
                xml_escape(&r.name),
                r.secs
            ));
        } else {
            out.push_str(&format!(
                "    <testcase name=\"{}\" classname=\"hdlsim\" time=\"{:.3}\">\n",
                xml_escape(&r.name),
                r.secs
            ));
            out.push_str(&format!(
                "      <failure message=\"{}\"/>\n",
                xml_escape(&r.detail)
            ));
            if let Some(cov) = &r.coverage {
                out.push_str(&format!(
                    "      <system-out>{}</system-out>\n",
                    xml_escape(cov)
                ));
            }
            out.push_str("    </testcase>\n");
        }
    }
    out.push_str("  </testsuite>\n</testsuites>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn junit_escapes_and_counts() {
        let results = vec![
            TestResult {
                name: "tb_ok".into(),
                passed: true,
                secs: 0.001,
                detail: String::new(),
                coverage: None,
            },
            TestResult {
                name: "tb_bad<1>".into(),
                passed: false,
                secs: 0.002,
                detail: "assert failed: \"x\" & <y>".into(),
                coverage: Some("coverage: line 1/2".into()),
            },
        ];
        let xml = junit_xml(&results, 0.003);
        assert!(xml.contains("tests=\"2\" failures=\"1\""), "{}", xml);
        assert!(xml.contains("name=\"tb_bad&lt;1&gt;\""), "{}", xml);
        assert!(xml.contains("&quot;x&quot; &amp; &lt;y&gt;"), "{}", xml);
        assert!(
            xml.contains("<system-out>coverage: line 1/2 (50.0%)</system-out>")
                || xml.contains("<system-out>coverage: line 1/2"),
            "{}",
            xml
        );
    }
}
