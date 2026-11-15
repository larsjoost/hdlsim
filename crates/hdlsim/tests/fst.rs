//! End-to-end tests for the FST wave format: `run --fst out.fst` writes a
//! GTKWave-compatible FST file, its decoded events match the `.hwf` trace of
//! the same run, and `hdlsim wave out.fst` renders it into the HTML viewer.

use std::collections::BTreeMap;
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
    std::env::temp_dir().join(format!("hdlsim_fst_{}_{}", std::process::id(), name))
}

// Tests run in parallel within one process; keep tmp files per test.
fn uniq(tag: &str) -> String {
    format!("{}_{}", tag, std::process::id())
}

/// Run the counter example with both wave outputs; return (hwf path, fst path).
fn run_counter(tag: &str) -> (PathBuf, PathBuf) {
    let ex = examples().join("counter");
    let tb = ex.join("tb_counter.v");
    let hwf = tmp(&format!("{}_c.hwf", uniq(tag)));
    let fst = tmp(&format!("{}_c.fst", uniq(tag)));
    let _ = std::fs::remove_file(&hwf);
    let _ = std::fs::remove_file(&fst);
    let out = hdlsim()
        .args([
            "run",
            "--top",
            "tb_counter",
            "-L",
            &format!("work={}", ex.display()),
            tb.to_str().unwrap(),
            "--hwf",
            hwf.to_str().unwrap(),
            "--fst",
            fst.to_str().unwrap(),
        ])
        .output()
        .expect("runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(hwf.is_file() && fst.is_file());
    (hwf, fst)
}

#[test]
fn fst_decoded_events_match_hwf() {
    let tag = "match";
    let (hwf, fst) = run_counter(tag);
    let hwf_bytes = std::fs::read(&hwf).unwrap();
    let fst_bytes = std::fs::read(&fst).unwrap();

    // Structural sanity of the FST container: header block, tag 0, len 329,
    // timescale -15 (1 fs) at offset 73.
    assert_eq!(fst_bytes[0], 0, "first block is the header");
    assert_eq!(
        u64::from_be_bytes(fst_bytes[1..9].try_into().unwrap()),
        329,
        "header section length"
    );
    assert_eq!(fst_bytes[73], (-15i8) as u8, "timescale 1fs");

    let (flat_h, ev_h) = hdl_wave::sinks::hwf::read_hwf(&hwf_bytes).unwrap();
    let (flat_f, ev_f) = hdl_wave::fst::read_fst(&fst_bytes).unwrap();

    // Same signal set (local names + widths), possibly in different handle order.
    // (.hwf keeps local names; .fst qualifies them with the scope path.)
    fn local(n: &str) -> &str {
        n.rsplit('.').next().unwrap_or(n)
    }
    let h: BTreeMap<&str, usize> = flat_h
        .signals
        .iter()
        .map(|s| (local(&s.name), s.width))
        .collect();
    let f: BTreeMap<&str, usize> = flat_f
        .signals
        .iter()
        .map(|s| (local(&s.name), s.width))
        .collect();
    assert_eq!(h, f, "same signals in hwf and fst");
    assert_eq!(h.len(), 5, "counter has 5 signals");
    // Hierarchy survived: the tb_counter instance `counter` is a nested scope.
    assert!(
        flat_f.scopes.len() >= 2 && flat_f.scopes.iter().any(|s| s.name == "counter"),
        "fst keeps the scope tree: {:?}",
        flat_f.scopes
    );

    // Same change stream, keyed by (signal name, time, new value bits).
    fn name_of(flat: &hdl_ir::elab::Flat, sig: u32) -> String {
        local(&flat.signals[sig as usize].name).to_string()
    }
    fn changes(
        flat: &hdl_ir::elab::Flat,
        evs: &[hdl_sim::events::SimEvent],
    ) -> Vec<(String, u64, Vec<u8>)> {
        let mut v = Vec::new();
        for ev in evs {
            if let hdl_sim::events::SimEvent::SignalChange {
                sig, new, time_fs, ..
            } = ev
            {
                v.push((name_of(flat, *sig), *time_fs, new.bits.clone()));
            }
        }
        v.sort();
        v
    }
    let ch = changes(&flat_h, &ev_h);
    let cf = changes(&flat_f, &ev_f);
    assert_eq!(ch.len(), cf.len(), "same number of value changes");
    assert!(!ch.is_empty(), "counter toggles signals");
    assert_eq!(ch, cf, "hwf and fst decode to the same changes");

    // Time advances cover the same range.
    fn times(evs: &[hdl_sim::events::SimEvent]) -> Vec<u64> {
        let mut v: Vec<u64> = evs
            .iter()
            .filter_map(|e| match e {
                hdl_sim::events::SimEvent::TimeAdvance { time_fs } => Some(*time_fs),
                _ => None,
            })
            .collect();
        v.sort();
        v.dedup();
        v
    }
    assert_eq!(times(&ev_h), times(&ev_f), "same time grid");

    let _ = std::fs::remove_file(&hwf);
    let _ = std::fs::remove_file(&fst);
}

#[test]
fn wave_command_renders_fst_to_html() {
    let tag = "render";
    let (hwf, fst) = run_counter(tag);
    let html = tmp(&format!("{}_c.html", uniq(tag)));
    let _ = std::fs::remove_file(&html);
    let out = hdlsim()
        .args(["wave", fst.to_str().unwrap(), "-o", html.to_str().unwrap()])
        .output()
        .expect("runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("5 signals"), "{}", stdout);
    let page = std::fs::read_to_string(&html).unwrap();
    assert!(page.contains("<title>hdlsim waves"), "html viewer title");
    assert!(page.contains("clk"), "signal name in page");
    assert!(page.contains("q"), "counter output in page");

    // A garbage .fst is rejected, not silently rendered.
    let bad = tmp(&format!("{}_bad.fst", uniq(tag)));
    std::fs::write(&bad, b"definitely not fst").unwrap();
    let out = hdlsim()
        .args(["wave", bad.to_str().unwrap()])
        .output()
        .expect("runs");
    assert!(!out.status.success());

    let _ = std::fs::remove_file(&hwf);
    let _ = std::fs::remove_file(&fst);
    let _ = std::fs::remove_file(&html);
    let _ = std::fs::remove_file(&bad);
}
