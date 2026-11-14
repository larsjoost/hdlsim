//! `hdlsim debug` — run a simulation with debugging output: dump the elaborated
//! design, watch signal changes live, and dump memory contents at the end.
//!
//! Also usable as a library: `hdl_sim::debug::Watcher` is an `Observer` you can
//! attach to any `Simulator` in your own tooling.

use super::{build_manager, extract_common, parse_time, Command};
use hdl_ir::elab;
use hdl_ir::Design;
use hdl_sim::debug::{fmt_time, fmt_value, Watcher};
use hdl_sim::{Observer, SimEvent, Simulator};
use std::path::PathBuf;

/// Prints `$display`/`report` messages to stdout, like a simulator transcript.
struct Transcript;

impl Observer for Transcript {
    fn on_event(&mut self, ev: &SimEvent) {
        if let SimEvent::Message { text, .. } = ev {
            print!("{}", text);
            use std::io::Write;
            let _ = std::io::stdout().flush();
        }
    }
}

pub struct Debug;

impl Command for Debug {
    fn name(&self) -> &'static str {
        "debug"
    }
    fn about(&self) -> &'static str {
        "debug a design: hdlsim debug --top tb [files] [--dump] [--watch sig]... [--all] [--mem name]... [--max-time 10us]"
    }
    fn run(&self, args: &[String]) -> Result<i32, String> {
        let (opts, rest) = extract_common(args)?;
        let mut files: Vec<PathBuf> = Vec::new();
        let mut top: Option<String> = None;
        let mut watches: Vec<String> = Vec::new();
        let mut mems: Vec<String> = Vec::new();
        let mut all = false;
        let mut dump = false;
        let mut max_time_fs = u64::MAX;
        let mut i = 0;
        while i < rest.len() {
            match rest[i].as_str() {
                "--top" => {
                    top = Some(rest.get(i + 1).ok_or("--top needs a value")?.clone());
                    i += 2;
                }
                "--watch" => {
                    watches.push(rest.get(i + 1).ok_or("--watch needs a value")?.clone());
                    i += 2;
                }
                "--mem" => {
                    mems.push(rest.get(i + 1).ok_or("--mem needs a value")?.clone());
                    i += 2;
                }
                "--all" => {
                    all = true;
                    i += 1;
                }
                "--dump" => {
                    dump = true;
                    i += 1;
                }
                "--max-time" => {
                    let v = rest.get(i + 1).ok_or("--max-time needs a value")?;
                    max_time_fs = parse_time(v)?;
                    i += 2;
                }
                other if other.starts_with("--") => {
                    return Err(format!("unknown flag '{}'", other));
                }
                f => {
                    files.push(PathBuf::from(f));
                    i += 1;
                }
            }
        }
        let top = top.ok_or(
            "debug needs --top TB (file arguments may be omitted when libraries are configured)",
        )?;
        let mut mgr = build_manager(&opts)?;
        let mut design = Design::default();
        mgr.compile_files(&mut design, &files);
        if mgr.diags().has_errors() {
            eprint!("{}", mgr.diags().render_all(mgr.db()));
            return Ok(1);
        }
        let flat = elab::elaborate_with(&mut design, &top, &mut mgr)
            .map_err(|e| format!("elaboration error: {}", e))?;
        if !mgr.diags().is_empty() {
            eprint!("{}", mgr.diags().render_all(mgr.db()));
        }
        let mut sim = Simulator::new(flat);
        if dump {
            print!("{}", sim.dump_design());
        }
        let mut watcher = Watcher::new(sim.flat()).echo(true);
        if all {
            watcher = watcher.watch_all();
        }
        for w in &watches {
            watcher = watcher.watch(w);
        }
        sim.add_observer(Box::new(watcher));
        sim.add_observer(Box::new(Transcript));
        sim.run(max_time_fs);
        println!("hdlsim debug: stopped at t={}", fmt_time(sim.time()));
        for m in &mems {
            let Some(id) = sim.signal_id(m) else {
                eprintln!("hdlsim debug: no signal named '{}'", m);
                continue;
            };
            if !sim.signal_is_mem(id) {
                println!(
                    "{} = {} (not a memory)",
                    sim.signal_name(id),
                    fmt_value(&sim.signal_value(id))
                );
                continue;
            }
            println!("mem {} ({} words):", sim.signal_name(id), sim.mem_len(id));
            for w in 0..sim.mem_len(id) {
                let v = sim.mem_word(id, w).unwrap();
                println!("  [{}] = {}", w, fmt_value(&v));
            }
        }
        Ok(0)
    }
}
