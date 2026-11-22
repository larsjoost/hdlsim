//! `hdlsim run` — compile (with libraries) and simulate one design.

use super::{build_manager, parse_time, parse_with, remap_into_bundle, Command, CommonArgs};
use clap::Parser;
use hdl_ir::elab;
use hdl_ir::Design;
use hdl_sim::{Observer, SimEvent, Simulator};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    about = "compile and simulate a testbench: hdlsim run --top tb [files] [--vcd w.vcd] [--max-time 10us]"
)]
struct RunArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// Top-level unit (default: the single compiled module).
    #[arg(long)]
    top: Option<String>,
    /// Write waves to this VCD file.
    #[arg(long)]
    vcd: Option<PathBuf>,
    /// Write waves to this .hwf file (view with `hdlsim wave`).
    #[arg(long)]
    hwf: Option<PathBuf>,
    /// Write waves to this FST file (view with `hdlsim wave`).
    #[arg(long)]
    fst: Option<PathBuf>,
    /// Write a standalone HTML wave-viewer page.
    #[arg(long)]
    wave: Option<PathBuf>,
    /// Open the live wave viewer in the browser.
    #[arg(long)]
    live: bool,
    /// Port for --live.
    #[arg(long, default_value_t = 8085)]
    port: u16,
    /// Stop time (e.g. 10us); default: run until there are no more events.
    #[arg(long)]
    max_time: Option<String>,
    /// Wall-clock limit for the whole run (e.g. 30s, 1.5m): on expiry the
    /// run stops with a location dump and exit code 124.
    #[arg(long)]
    max_wall: Option<String>,
    /// Save the simulator state here when the run stops (resume with --restore).
    #[arg(long)]
    checkpoint: Option<PathBuf>,
    /// Start from a checkpoint saved by an earlier --checkpoint run.
    #[arg(long)]
    restore: Option<PathBuf>,
    /// Collect code coverage during the run and report it at the end.
    #[arg(long)]
    coverage: bool,
    /// Write the coverage report to this file (default: stdout).
    #[arg(long)]
    coverage_out: Option<PathBuf>,
    /// Seed for random stimulus ($random, $urandom): same seed, same sequence.
    #[arg(long)]
    seed: Option<u64>,
    /// Source files.
    files: Vec<PathBuf>,
}

/// Prints `$display`/`report` messages to stdout, like a simulator transcript.
pub(crate) struct Transcript;

impl Observer for Transcript {
    fn on_event(&mut self, ev: &SimEvent) {
        let color = hdl_syntax::diagnostic::color_enabled();
        match ev {
            SimEvent::Message { text, .. } => {
                // Colorize severity-ish message lines ($error/$fatal/assert
                // failures, warnings); plain $display output stays uncolored.
                let t = text.trim_start();
                let painted = if t.starts_with("error")
                    || t.starts_with("fatal")
                    || t.contains("assertion failed")
                {
                    hdl_syntax::diagnostic::paint(color, "1;31", text)
                } else if t.starts_with("warning") {
                    hdl_syntax::diagnostic::paint(color, "1;33", text)
                } else {
                    text.clone()
                };
                print!("{}", painted);
                use std::io::Write;
                let _ = std::io::stdout().flush();
            }
            // TestFailed is not printed here: its text also arrives as a
            // Message (colorized above) — printing both would duplicate it.
            _ => {}
        }
    }
}

pub struct Run;

impl Command for Run {
    fn name(&self) -> &'static str {
        "run"
    }
    fn about(&self) -> &'static str {
        "compile and simulate a testbench: hdlsim run --top tb [files] [--vcd w.vcd] [--max-time 10us]"
    }
    fn run(&self, args: &[String]) -> anyhow::Result<i32> {
        let a = match parse_with::<RunArgs>("run", args) {
            Ok(a) => a,
            Err(code) => return Ok(code),
        };
        let opts = a.common.into_opts()?;
        let top = a.top;
        let vcd = a.vcd;
        let hwf = a.hwf;
        let fst = a.fst;
        let wave = a.wave;
        let live = a.live;
        let port = a.port;
        let max_time_fs = match &a.max_time {
            Some(v) => parse_time(v)?,
            None => u64::MAX,
        };
        let max_wall = match &a.max_wall {
            Some(v) => Some(crate::commands::parse_wall_secs(v)?),
            None => None,
        };
        let checkpoint = a.checkpoint;
        let restore = a.restore;
        let coverage = a.coverage;
        let coverage_out = a.coverage_out;
        let seed = a.seed;
        let files: Vec<PathBuf> = a.files;
        // Files may be omitted when --top is given: the top (and its
        // dependencies) are then resolved on demand from the libraries.
        if files.is_empty() && top.is_none() {
            return Err(anyhow::anyhow!(
                "no input files (or pass --top to resolve from libraries)"
            ));
        }
        // Compile the explicit files, then let elaboration pull in any missing
        // design units from the configured libraries on demand.
        let (mut mgr, root) = build_manager(&opts)?;
        let files: Vec<PathBuf> = files.iter().map(|f| remap_into_bundle(f, &root)).collect();
        let mut design = Design::default();
        let compile_phase = crate::phase::Phase::start("compile");
        mgr.compile_files(&mut design, &files);
        compile_phase.done(format!("{} modules", design.modules.len()));
        if mgr.diags().has_errors() {
            eprint!("{}", mgr.diags().render_all(mgr.db()));
            return Ok(1);
        }
        if crate::phase::over_mem_limit("compile") {
            return Ok(1);
        }
        let top_name = match top {
            Some(t) => t,
            None => super::pick_default_top(&design)?,
        };
        let elab_phase = crate::phase::Phase::start("elaborate");
        let flat = elab::elaborate_with(&mut design, &top_name, &mut mgr)
            .map_err(|e| anyhow::anyhow!("elaboration error: {}", e))?;
        elab_phase.done(format!(
            "{} signals, {} processes, {} assigns, max-mem-depth {}",
            flat.signals.len(),
            flat.procs.len(),
            flat.assigns.len(),
            flat.signals.iter().filter_map(|s| s.mem).max().unwrap_or(0)
        ));
        if crate::phase::over_mem_limit("elaborate") {
            return Ok(1);
        }
        if !mgr.diags().is_empty() {
            eprint!("{}", mgr.diags().render_all(mgr.db()));
        }
        if mgr.diags().has_errors() {
            // On-demand resolution compiles dependencies during elaboration;
            // their errors must stop the run here — simulating a design that
            // failed to compile produces garbage results (or hangs).
            return Ok(1);
        }
        let init_phase = crate::phase::Phase::start("sim-init");
        let mut sim = match &restore {
            Some(p) => Simulator::restore_checkpoint(flat, p).map_err(anyhow::Error::msg)?,
            None => {
                // Refuse runaway allocations with the top-allocator report
                // instead of aborting inside the first big `vec![…]`.
                Simulator::alloc_report(&flat).map_err(anyhow::Error::msg)?;
                Simulator::new(flat)
            }
        };
        init_phase.done("state initialized");
        if crate::phase::over_mem_limit("sim-init") {
            return Ok(1);
        }
        if let Some(s) = seed {
            sim.set_seed(s);
        }
        sim.add_observer(Box::new(Transcript));
        // Coverage collector, kept behind a shared handle so we can report
        // after the run.
        let cov = if coverage {
            let widths: Vec<usize> = sim.flat().signals.iter().map(|s| s.width).collect();
            let c = std::rc::Rc::new(std::cell::RefCell::new(
                hdl_sim::coverage::CoverageCollector::new(&design, &widths),
            ));
            sim.add_observer(Box::new(hdl_sim::coverage::SharedCollector(c.clone())));
            Some(c)
        } else {
            None
        };
        if let Some(path) = vcd {
            let file = std::fs::File::create(&path)
                .map_err(|e| anyhow::anyhow!("cannot create {}: {}", path.display(), e))?;
            let sink = hdl_wave::sinks::vcd::VcdSink::new(file, sim.flat());
            sim.add_observer(Box::new(hdl_wave::SinkAdapter {
                sink,
                flat: sim.flat().clone(),
            }));
        }
        if let Some(path) = hwf {
            let file = std::fs::File::create(&path)
                .map_err(|e| anyhow::anyhow!("cannot create {}: {}", path.display(), e))?;
            let sink = hdl_wave::sinks::hwf::HwfSink::new(file, sim.flat());
            sim.add_observer(Box::new(hdl_wave::SinkAdapter {
                sink,
                flat: sim.flat().clone(),
            }));
            println!("hdlsim: .hwf waves will be written to {}", path.display());
        }
        if let Some(path) = fst {
            let file = std::fs::File::create(&path)
                .map_err(|e| anyhow::anyhow!("cannot create {}: {}", path.display(), e))?;
            let sink = hdl_wave::sinks::fst::FstSink::new(file, sim.flat());
            sim.add_observer(Box::new(hdl_wave::SinkAdapter {
                sink,
                flat: sim.flat().clone(),
            }));
            println!("hdlsim: .fst waves will be written to {}", path.display());
        }
        if let Some(path) = wave {
            let file = std::fs::File::create(&path)
                .map_err(|e| anyhow::anyhow!("cannot create {}: {}", path.display(), e))?;
            let sink = hdl_wave::sinks::html::HtmlSink::new(file, path.clone(), sim.flat());
            sim.add_observer(Box::new(hdl_wave::SinkAdapter {
                sink,
                flat: sim.flat().clone(),
            }));
            println!("hdlsim: wave viewer will be written to {}", path.display());
        }
        let (live_bus, live_port) = if live {
            let bus = hdl_wave::sinks::live::LiveBus::new(sim.flat());
            let (bound, _server) = hdl_wave::sinks::live::spawn_live_server(bus.clone(), port)
                .map_err(|e| anyhow::anyhow!("cannot start live server on port {}: {}", port, e))?;
            sim.set_stop_flag(bus.stop_flag());
            sim.add_observer(Box::new(hdl_wave::SinkAdapter {
                sink: hdl_wave::sinks::live::LiveSink::new(bus.clone()),
                flat: sim.flat().clone(),
            }));
            println!(
                "hdlsim: live waves at http://127.0.0.1:{}/ (Ctrl-C or Stop button to end)",
                bound
            );
            (Some(bus), Some(bound))
        } else {
            (None, None)
        };
        // Ctrl-C / SIGTERM: the handler only sets a flag (safe from the
        // signal context); the kernel dumps the location at a safe point.
        let interrupt_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let f = interrupt_flag.clone();
            if ctrlc::set_handler(move || {
                f.store(true, std::sync::atomic::Ordering::Relaxed)
            })
            .is_err()
            {
                eprintln!("hdlsim: note: Ctrl-C handler unavailable; interrupt dumps disabled");
            }
        }
        sim.set_interrupt_flag(interrupt_flag.clone(), 130);
        if let Some(secs) = max_wall {
            sim.set_wall_budget(std::time::Duration::from_secs_f64(secs));
        }
        let sim_phase = crate::phase::Phase::start("simulate");
        sim.run(max_time_fs);
        sim_phase.done(format!("t={}", hdl_ir::fmt_time(sim.time())));
        println!(
            "hdlsim: simulation stopped at t={} ({} signals, {} processes)",
            hdl_ir::fmt_time(sim.time()),
            sim.flat().signals.len(),
            sim.flat().procs.len()
        );
        // Ended by SIGINT/SIGTERM or --max-wall: the dump is already in the
        // transcript; map it to the conventional exit code (130 / 124).
        if let Some((reason, rc)) = sim.interrupt_info() {
            println!("hdlsim: {reason} — exit code {rc}");
            return Ok(rc);
        }
        if let Some(p) = &checkpoint {
            sim.save_checkpoint(p)
                .map_err(|e| anyhow::anyhow!("cannot write checkpoint {}: {}", p.display(), e))?;
            println!("hdlsim: checkpoint written to {}", p.display());
        }
        if let Some(cov) = &cov {
            let report = cov.borrow().report(mgr.db());
            match &coverage_out {
                Some(p) => {
                    std::fs::write(p, &report)
                        .map_err(|e| anyhow::anyhow!("cannot write {}: {}", p.display(), e))?;
                    println!("hdlsim: coverage report written to {}", p.display());
                }
                None => print!("{}", report),
            }
        }
        // A failed unit test (assert/report severity error|failure, $error/$fatal)
        // makes the whole run fail: exit code 1 so CI/scripts catch it.
        if sim.failures() > 0 {
            println!(
                "{}",
                hdl_syntax::diagnostic::paint(
                    hdl_syntax::diagnostic::color_enabled(),
                    "1;31",
                    &format!("hdlsim: FAILED — {} test failure(s)", sim.failures())
                )
            );
            return Ok(1);
        }
        if sim.error_stopped() {
            // Stopped by an internal guard (combinational loop, delta-cycle or
            // process-step limit): the transcript already carries the `error:`
            // line — fail the run so CI does not read a truncated run as pass.
            return Ok(1);
        }
        // std.env.finish(status): the STATUS argument becomes the process exit
        // code (OS exit codes are 0..=255; values are truncated to that range).
        if let Some(status) = sim.finish_status() {
            return Ok((status & 0xFF) as i32);
        }
        if let Some(bus) = &live_bus {
            // End the SSE streams (they drain their backlog first), but keep
            // the HTTP server up so the finished waves stay in the browser
            // until Ctrl-C.
            bus.close();
            if let Some(port) = live_port {
                println!(
                    "hdlsim: waves still viewable at http://127.0.0.1:{}/ — press Ctrl-C to exit",
                    port
                );
                // The Ctrl-C handler is flag-based, so watch the flag here
                // (short sleep slices) instead of relying on the default
                // signal death to end the server.
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(250));
                    if interrupt_flag.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                }
            }
        }
        Ok(0)
    }
}
