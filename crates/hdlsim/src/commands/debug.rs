//! `hdlsim debug` — run a simulation with debugging output: dump the elaborated
//! design, watch signal changes live, and dump memory contents at the end.
//!
//! Also usable as a library: `hdl_sim::debug::Watcher` is an `Observer` you can
//! attach to any `Simulator` in your own tooling.

use super::{build_manager, extract_common, parse_time, parse_with, remap_into_bundle, Command};
use clap::Parser;
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

#[derive(Parser)]
#[command(
    about = "inspect the simulated design: hdlsim debug --top tb [--watch sig]... [--driver d]... [--mem m]... [--all] [--dump] [--max-time T] [--seed N] [files...]"
)]
struct DebugArgs {
    /// Top-level unit.
    #[arg(long)]
    top: Option<String>,
    /// Signal to watch.
    #[arg(long)]
    watch: Vec<String>,
    /// Driver to trace.
    #[arg(long)]
    driver: Vec<String>,
    /// Memory to dump.
    #[arg(long)]
    mem: Vec<String>,
    /// Dump/watch everything.
    #[arg(long)]
    all: bool,
    /// Dump the elaborated design.
    #[arg(long)]
    dump: bool,
    /// Stop time (e.g. 10us).
    #[arg(long)]
    max_time: Option<String>,
    /// RNG seed.
    #[arg(long)]
    seed: Option<u64>,
    /// Source files.
    files: Vec<PathBuf>,
}

pub struct Debug;

impl Command for Debug {
    fn name(&self) -> &'static str {
        "debug"
    }
    fn about(&self) -> &'static str {
        "debug a design: hdlsim debug --top tb [files] [--dump] [--watch sig]... [--all] [--mem name]... [--max-time 10us] [--seed N]"
    }
    fn run(&self, args: &[String]) -> Result<i32, String> {
        let (opts, rest) = extract_common(args)?;
        let a = match parse_with::<DebugArgs>("debug", &rest) {
            Ok(a) => a,
            Err(code) => return Ok(code),
        };
        let top = a.top.ok_or(
            "debug needs --top TB (file arguments may be omitted when libraries are configured)",
        )?;
        let watches = a.watch;
        let drivers = a.driver;
        let mems = a.mem;
        let all = a.all;
        let dump = a.dump;
        let max_time_fs = match &a.max_time {
            Some(v) => parse_time(v)?,
            None => u64::MAX,
        };
        let seed = a.seed;
        let files: Vec<PathBuf> = a.files;
        let (mut mgr, root) = build_manager(&opts)?;
        let files: Vec<PathBuf> = files.iter().map(|f| remap_into_bundle(f, &root)).collect();
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
        if let Some(s) = seed {
            sim.set_seed(s);
        }
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
        if sim.failures() > 0 {
            println!("hdlsim debug: FAILED — {} test failure(s)", sim.failures());
            return Ok(1);
        }
        for d in &drivers {
            let Some(id) = sim.signal_id(d) else {
                eprintln!("hdlsim debug: no signal named '{}'", d);
                continue;
            };
            match sim.driver_of(id) {
                Some((t, origin)) => println!(
                    "driver of {}: {} (last write at t={})",
                    sim.signal_name(id),
                    origin,
                    fmt_time(t)
                ),
                None => println!("driver of {}: never driven", sim.signal_name(id)),
            }
        }
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
