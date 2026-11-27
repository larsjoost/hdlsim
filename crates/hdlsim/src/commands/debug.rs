//! `hdlsim debug` — run a simulation with debugging output: dump the elaborated
//! design, watch signal changes live, and dump memory contents at the end.
//!
//! Also usable as a library: `hdl_sim::debug::Watcher` is an `Observer` you can
//! attach to any `Simulator` in your own tooling.

use super::run::Transcript;
use super::{
    build_manager, emit_diags, parse_time, parse_with, remap_into_bundle, Command, CommonArgs,
};
use clap::Parser;
use hdl_ir::elab;
use hdl_ir::Design;
use hdl_sim::debug::{fmt_time, fmt_value, Watcher};
use hdl_sim::Simulator;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    about = "inspect the simulated design: hdlsim debug --top tb [--watch sig]... [--driver d]... [--mem m]... [--all] [--dump] [--max-time T] [--seed N] [files...]"
)]
struct DebugArgs {
    #[command(flatten)]
    common: CommonArgs,
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
    /// Echo a line when a signal changes and the rule matches:
    /// `--when 'sig=value@T0-T1'` (same spec grammar as `run --when`).
    /// Repeatable.
    #[arg(long, value_name = "SPEC")]
    when: Vec<String>,
    /// Print a signal's settled value once: `--examine 'sig@2us'` at exactly
    /// 2us; bare `--examine sig` at end of run. Repeatable.
    #[arg(long, value_name = "SPEC")]
    examine: Vec<String>,
    /// Log every evaluation of the continuous assignments driving the named
    /// signals, with the values the RHS read (same output as
    /// `run --trace-assign`). Repeatable.
    #[arg(long, value_name = "NAMES")]
    trace_assign: Vec<String>,
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
        "inspect a simulation — dump the design, watch signals, dump memories: hdlsim debug --top tb [files] --dump"
    }
    fn run(&self, args: &[String]) -> anyhow::Result<i32> {
        let a = match parse_with::<DebugArgs>("debug", args) {
            Ok(a) => a,
            Err(code) => return Ok(code),
        };
        let opts = a.common.into_opts()?;
        let top = a.top.ok_or(
            "debug needs --top TB (file arguments may be omitted when libraries are configured)",
        ).map_err(anyhow::Error::msg)?;
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
        let when = a.when;
        let examine = a.examine;
        let files: Vec<PathBuf> = a.files;
        let (mut mgr, root) = build_manager(&opts)?;
        let files: Vec<PathBuf> = files.iter().map(|f| remap_into_bundle(f, &root)).collect();
        let mut design = Design::default();
        let compile_phase = crate::phase::Phase::start("compile");
        mgr.compile_files(&mut design, &files);
        compile_phase.done(format!("{} modules", design.modules.len()));
        if mgr.diags().has_errors() {
            emit_diags(mgr.diags(), mgr.db());
            return Ok(1);
        }
        let elab_phase = crate::phase::Phase::start("elaborate");
        let flat = elab::elaborate_with(&mut design, &top, &mut mgr)
            .map_err(|e| anyhow::anyhow!("elaboration error: {}", e))?;
        elab_phase.done(format!(
            "{} signals, {} processes, {} assigns",
            flat.signals.len(),
            flat.procs.len(),
            flat.assigns.len()
        ));
        if !mgr.diags().is_empty() {
            emit_diags(mgr.diags(), mgr.db());
        }
        // Refuse runaway allocations with the top-allocator report instead
        // of aborting inside the first big `vec![…]`.
        Simulator::alloc_report(&flat).map_err(anyhow::Error::msg)?;
        let init_phase = crate::phase::Phase::start("sim-init");
        let mut sim = Simulator::new(flat);
        if let Some(s) = seed {
            sim.set_seed(s);
        }
        sim.set_when_rules(&when).map_err(anyhow::Error::msg)?;
        sim.set_examine_rules(&examine)
            .map_err(anyhow::Error::msg)?;
        sim.set_trace_assigns(&a.trace_assign)
            .map_err(anyhow::Error::msg)?;
        init_phase.done("state initialized");
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
        sim.add_observer(Box::new(Transcript::new(mgr.db().clone())));
        let sim_phase = crate::phase::Phase::start("simulate");
        sim.run(max_time_fs);
        sim.examine_flush_final();
        sim_phase.done(format!("t={}", fmt_time(sim.time())));
        println!("hdlsim debug: stopped at t={}", fmt_time(sim.time()));
        if sim.failures() > 0 {
            let mut msg = format!("hdlsim debug: FAILED — {} test failure(s)", sim.failures());
            if let Some(span) = sim.first_failure_span() {
                msg = format!("{} (first at {})", msg, super::span_loc(mgr.db(), span));
            }
            println!("{}", msg);
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
                let v = sim
                    .mem_word(id, w)
                    .expect("w < mem_len(id), so the word exists");
                println!("  [{}] = {}", w, fmt_value(&v));
            }
        }
        Ok(0)
    }
}
