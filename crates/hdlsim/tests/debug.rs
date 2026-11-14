//! Tests for the debugging API: `hdl_sim::debug` (value formatting, the
//! `Watcher` observer) and the `Simulator` introspection methods, plus the
//! `hdlsim debug` CLI command end-to-end on the counter example.

use hdl_ir::elab;
use hdl_ir::Value;
use hdl_sim::debug::{fmt_time, fmt_value, Watcher};
use hdl_sim::{Observer, SimEvent, Simulator};
use hdlsim::compile::compile_paths;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

fn counter_files() -> Vec<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/counter");
    vec![dir.join("counter.v"), dir.join("tb_counter.v")]
}

/// Forwards events into a `Watcher` and copies its lines out (Observer must be
/// 'static, so the watcher itself cannot be borrowed after the run).
struct WatchProbe {
    watcher: Watcher,
    lines: Arc<Mutex<Vec<String>>>,
}

impl Observer for WatchProbe {
    fn on_event(&mut self, ev: &SimEvent) {
        let before = self.watcher.lines().len();
        self.watcher.on_event(ev);
        let mut out = self.lines.lock().unwrap();
        out.extend(self.watcher.lines()[before..].iter().cloned());
    }
}

#[test]
fn fmt_value_hex_and_bits() {
    assert_eq!(fmt_value(&Value::from_u64(8, 0xab)), "0xAB");
    assert_eq!(fmt_value(&Value::from_u64(1, 1)), "0x1");
    let mut v = Value::from_u64(4, 0b1010);
    v.bits[2] = hdl_ir::value::VX;
    assert_eq!(fmt_value(&v), "1x10"); // bits[0] is LSB, printed rightmost
                                       // Wide clean values are truncated to the low 64 bits.
    let wide = Value::from_u64(128, 0xFFFF);
    assert!(fmt_value(&wide).ends_with("(128 bits)"));
}

#[test]
fn fmt_time_units() {
    assert_eq!(fmt_time(500), "500fs");
    assert_eq!(fmt_time(5_000_000), "5ns");
    assert_eq!(fmt_time(10_000_000_000), "10us");
    assert_eq!(fmt_time(2_000_000_000_000), "2ms");
}

#[test]
fn watcher_records_watched_signal_changes() {
    let compiled = compile_paths(&counter_files());
    assert!(!compiled.has_errors(), "{}", compiled.render_diags());
    let flat = elab::elaborate(&compiled.design, "tb_counter").unwrap();
    let mut sim = Simulator::new(flat);
    let lines = Arc::new(Mutex::new(Vec::new()));
    let probe = WatchProbe {
        watcher: Watcher::new(sim.flat()).watch("clk"),
        lines: lines.clone(),
    };
    sim.add_observer(Box::new(probe));
    sim.run(1_000_000_000);
    let got = lines.lock().unwrap();
    assert!(
        got.iter().any(|l| l.contains("clk = 0x1")),
        "lines: {:?}",
        *got
    );
    assert!(
        got.iter().any(|l| l.contains("clk = 0x0")),
        "lines: {:?}",
        *got
    );
    // Everything recorded is either a clk line or a transcript/finish marker.
    assert!(got
        .iter()
        .all(|l| l.contains("clk") || l.contains('$') || l.contains('|')));
}

#[test]
fn watcher_matches_sig_id_and_leaf() {
    let compiled = compile_paths(&counter_files());
    let flat = elab::elaborate(&compiled.design, "tb_counter").unwrap();
    let mut sim = Simulator::new(flat);
    let clk_id = sim.signal_id("clk").unwrap();
    let lines = Arc::new(Mutex::new(Vec::new()));
    let probe = WatchProbe {
        watcher: Watcher::new(sim.flat()).watch(&format!("#{}", clk_id)),
        lines: lines.clone(),
    };
    sim.add_observer(Box::new(probe));
    sim.run(20_000_000);
    assert!(!lines.lock().unwrap().is_empty());
}

#[test]
fn simulator_introspection_api() {
    let compiled = compile_paths(&counter_files());
    let flat = elab::elaborate(&compiled.design, "tb_counter").unwrap();
    let mut sim = Simulator::new(flat);
    let clk = sim.signal_id("clk").expect("clk signal exists");
    assert_eq!(sim.signal_name(clk), "clk");
    assert_eq!(sim.signal_width(clk), 1);
    assert!(sim.signal_id("#0").is_some());
    assert!(sim.signal_id("no_such_signal").is_none());
    assert!(!sim.signal_is_mem(clk));
    let dump = sim.dump_design();
    assert!(dump.contains("clk"), "dump:\n{}", dump);
    assert!(sim.proc_count() >= 2);
    assert!(dump.contains("proc #"));
    sim.run(50_000_000);
    let _ = sim.signal_value(clk);
}

#[test]
fn debug_command_runs_end_to_end() {
    let files = counter_files();
    let mut args: Vec<String> = vec![
        "debug".to_string(),
        "--top".to_string(),
        "tb_counter".to_string(),
        "--watch".to_string(),
        "clk".to_string(),
        "--max-time".to_string(),
        "20ns".to_string(),
    ];
    args.extend(files.iter().map(|p| p.to_string_lossy().into_owned()));
    let code = hdlsim::commands::dispatch(&args).unwrap();
    assert_eq!(code, 0);
}
