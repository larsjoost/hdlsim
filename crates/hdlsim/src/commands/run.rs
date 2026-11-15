//! `hdlsim run` — compile (with libraries) and simulate one design.

use super::{build_manager, extract_common, parse_time, remap_into_bundle, Command};
use hdl_ir::elab;
use hdl_ir::Design;
use hdl_sim::{Observer, SimEvent, Simulator};
use std::path::PathBuf;

/// Prints `$display`/`report` messages to stdout, like a simulator transcript.
pub(crate) struct Transcript;

impl Observer for Transcript {
    fn on_event(&mut self, ev: &SimEvent) {
        if let SimEvent::Message { text, .. } = ev {
            print!("{}", text);
            use std::io::Write;
            let _ = std::io::stdout().flush();
        }
    }
}

pub struct Run;

impl Command for Run {
    fn name(&self) -> &'static str {
        "run"
    }
    fn about(&self) -> &'static str {
        "compile and simulate: hdlsim run --top tb [files] [--vcd out.vcd] [--hwf out.hwf] [--fst out.fst] [--wave out.html] [--live [--port N]] [--max-time 10us] [--seed N]"
    }
    fn run(&self, args: &[String]) -> Result<i32, String> {
        let (opts, rest) = extract_common(args)?;
        let mut files: Vec<PathBuf> = Vec::new();
        let mut top: Option<String> = None;
        let mut vcd: Option<PathBuf> = None;
        let mut hwf: Option<PathBuf> = None;
        let mut fst: Option<PathBuf> = None;
        let mut wave: Option<PathBuf> = None;
        let mut live = false;
        let mut port: u16 = 8085;
        let mut max_time_fs = u64::MAX;
        let mut coverage = false;
        let mut coverage_out: Option<PathBuf> = None;
        let mut checkpoint: Option<PathBuf> = None;
        let mut restore: Option<PathBuf> = None;
        let mut seed: Option<u64> = None;
        let mut i = 0;
        while i < rest.len() {
            match rest[i].as_str() {
                "--top" => {
                    top = Some(rest.get(i + 1).ok_or("--top needs a value")?.clone());
                    i += 2;
                }
                "--vcd" => {
                    vcd = Some(PathBuf::from(rest.get(i + 1).ok_or("--vcd needs a value")?));
                    i += 2;
                }
                "--hwf" => {
                    hwf = Some(PathBuf::from(rest.get(i + 1).ok_or("--hwf needs a value")?));
                    i += 2;
                }
                "--fst" => {
                    fst = Some(PathBuf::from(rest.get(i + 1).ok_or("--fst needs a value")?));
                    i += 2;
                }
                "--wave" => {
                    wave = Some(PathBuf::from(
                        rest.get(i + 1).ok_or("--wave needs a value")?,
                    ));
                    i += 2;
                }
                "--live" => {
                    live = true;
                    i += 1;
                }
                "--port" => {
                    port = rest
                        .get(i + 1)
                        .ok_or("--port needs a value")?
                        .parse()
                        .map_err(|_| "--port needs a number")?;
                    i += 2;
                }
                "--max-time" => {
                    let v = rest.get(i + 1).ok_or("--max-time needs a value")?;
                    max_time_fs = parse_time(v)?;
                    i += 2;
                }
                "--checkpoint" => {
                    checkpoint = Some(PathBuf::from(
                        rest.get(i + 1).ok_or("--checkpoint needs a value")?,
                    ));
                    i += 2;
                }
                "--restore" => {
                    restore = Some(PathBuf::from(
                        rest.get(i + 1).ok_or("--restore needs a value")?,
                    ));
                    i += 2;
                }
                "--coverage" => {
                    coverage = true;
                    i += 1;
                }
                "--coverage-out" => {
                    coverage_out = Some(PathBuf::from(
                        rest.get(i + 1).ok_or("--coverage-out needs a value")?,
                    ));
                    i += 2;
                }
                "--seed" => {
                    seed = Some(
                        rest.get(i + 1)
                            .ok_or("--seed needs a value")?
                            .parse()
                            .map_err(|_| "--seed needs a u64")?,
                    );
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
        // Files may be omitted when --top is given: the top (and its
        // dependencies) are then resolved on demand from the libraries.
        if files.is_empty() && top.is_none() {
            return Err("no input files (or pass --top to resolve from libraries)".to_string());
        }
        // Compile the explicit files, then let elaboration pull in any missing
        // design units from the configured libraries on demand.
        let (mut mgr, root) = build_manager(&opts)?;
        let files: Vec<PathBuf> = files.iter().map(|f| remap_into_bundle(f, &root)).collect();
        let mut design = Design::default();
        mgr.compile_files(&mut design, &files);
        if mgr.diags().has_errors() {
            eprint!("{}", mgr.diags().render_all(mgr.db()));
            return Ok(1);
        }
        let top_name = match top {
            Some(t) => t,
            None => {
                if design.modules.len() == 1 {
                    design.modules[0].name.clone()
                } else {
                    let names: Vec<&str> = design.modules.iter().map(|m| m.name.as_str()).collect();
                    return Err(format!(
                        "multiple modules ({}); pick one with --top",
                        names.join(", ")
                    ));
                }
            }
        };
        let flat = elab::elaborate_with(&mut design, &top_name, &mut mgr)
            .map_err(|e| format!("elaboration error: {}", e))?;
        if !mgr.diags().is_empty() {
            eprint!("{}", mgr.diags().render_all(mgr.db()));
        }
        let mut sim = match &restore {
            Some(p) => Simulator::restore_checkpoint(flat, p)?,
            None => Simulator::new(flat),
        };
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
                .map_err(|e| format!("cannot create {}: {}", path.display(), e))?;
            let sink = hdl_wave::sinks::vcd::VcdSink::new(file, sim.flat());
            sim.add_observer(Box::new(hdl_wave::SinkAdapter {
                sink,
                flat: sim.flat().clone(),
            }));
        }
        if let Some(path) = hwf {
            let file = std::fs::File::create(&path)
                .map_err(|e| format!("cannot create {}: {}", path.display(), e))?;
            let sink = hdl_wave::sinks::hwf::HwfSink::new(file, sim.flat());
            sim.add_observer(Box::new(hdl_wave::SinkAdapter {
                sink,
                flat: sim.flat().clone(),
            }));
            println!("hdlsim: .hwf waves will be written to {}", path.display());
        }
        if let Some(path) = fst {
            let file = std::fs::File::create(&path)
                .map_err(|e| format!("cannot create {}: {}", path.display(), e))?;
            let sink = hdl_wave::sinks::fst::FstSink::new(file, sim.flat());
            sim.add_observer(Box::new(hdl_wave::SinkAdapter {
                sink,
                flat: sim.flat().clone(),
            }));
            println!("hdlsim: .fst waves will be written to {}", path.display());
        }
        if let Some(path) = wave {
            let file = std::fs::File::create(&path)
                .map_err(|e| format!("cannot create {}: {}", path.display(), e))?;
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
                .map_err(|e| format!("cannot start live server on port {}: {}", port, e))?;
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
        sim.run(max_time_fs);
        println!(
            "hdlsim: simulation stopped at t={} fs ({} signals, {} processes)",
            sim.time(),
            sim.flat().signals.len(),
            sim.flat().procs.len()
        );
        if let Some(p) = &checkpoint {
            sim.save_checkpoint(p)
                .map_err(|e| format!("cannot write checkpoint {}: {}", p.display(), e))?;
            println!("hdlsim: checkpoint written to {}", p.display());
        }
        if let Some(cov) = &cov {
            let report = cov.borrow().report(mgr.db());
            match &coverage_out {
                Some(p) => {
                    std::fs::write(p, &report)
                        .map_err(|e| format!("cannot write {}: {}", p.display(), e))?;
                    println!("hdlsim: coverage report written to {}", p.display());
                }
                None => print!("{}", report),
            }
        }
        // A failed unit test (assert/report severity error|failure, $error/$fatal)
        // makes the whole run fail: exit code 1 so CI/scripts catch it.
        if sim.failures() > 0 {
            println!("hdlsim: FAILED — {} test failure(s)", sim.failures());
            return Ok(1);
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
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(3600));
                }
            }
        }
        Ok(0)
    }
}
