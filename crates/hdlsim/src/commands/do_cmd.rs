//! `hdlsim do` — run a mini-Tcl do-file against a simulated design
//! (phase 2 of docs/plan.md).
//!
//! Usage: `hdlsim do script.do --top tb [--max-time 10us] [files...]`
//! (plus the common `-L`/`--config`/`-I` flags). The Tcl subset lives in
//! `hdlsim::tcl`; simulator commands:
//! - `run [time|-all]` — advance the sim (incremental; `run 100ns` steps)
//! - `restart` — rebuild the simulation from scratch
//! - `vcd dumpfile PATH` — attach a VCD sink from this point on
//! - `examine NAME` — print a signal's current value
//! - `quit`/`exit [code]` — stop the script (Tcl builtin)

use super::run::Transcript;
use super::{build_manager, extract_common, parse_time, remap_into_bundle, Command};
use crate::tcl::{Flow, Interp};
use hdl_ir::elab;
use hdl_ir::Design;
use hdl_lib::LibraryManager;
use hdl_sim::Simulator;
use std::path::PathBuf;

pub struct DoCmd;

impl Command for DoCmd {
    fn name(&self) -> &'static str {
        "do"
    }
    fn about(&self) -> &'static str {
        "run a mini-Tcl do-file: hdlsim do script.do --top tb [files]"
    }
    fn run(&self, args: &[String]) -> Result<i32, String> {
        let (opts, rest) = extract_common(args)?;
        let mut script: Option<PathBuf> = None;
        let mut top: Option<String> = None;
        let mut max_time_fs = u64::MAX;
        let mut files: Vec<PathBuf> = Vec::new();
        let mut i = 0;
        while i < rest.len() {
            match rest[i].as_str() {
                "--top" => {
                    top = Some(rest.get(i + 1).ok_or("--top needs a value")?.clone());
                    i += 2;
                }
                "--max-time" => {
                    max_time_fs = parse_time(rest.get(i + 1).ok_or("--max-time needs a value")?)?;
                    i += 2;
                }
                other if other.starts_with("--") => {
                    return Err(format!("unknown flag '{}'", other));
                }
                f => {
                    if script.is_none() {
                        script = Some(PathBuf::from(f));
                    } else {
                        files.push(PathBuf::from(f));
                    }
                    i += 1;
                }
            }
        }
        let script = script.ok_or("usage: hdlsim do <script.do> --top <tb> [files...]")?;
        let top = top.ok_or("--top is required")?;
        let text = std::fs::read_to_string(&script)
            .map_err(|e| format!("cannot read {}: {}", script.display(), e))?;
        let (mgr, root) = build_manager(&opts)?;
        let files: Vec<PathBuf> = files.iter().map(|f| remap_into_bundle(f, &root)).collect();

        let mut session = DoSession {
            mgr,
            files,
            top,
            max_time_fs,
            sim: None,
            vcd_path: None,
        };
        let mut interp = Interp::new();
        let flow = interp.eval(&text, &mut |_i, args| session.command(args))?;
        let code = match flow {
            Flow::Quit(c) => c,
            _ => 0,
        };
        if code != 0 {
            return Ok(code);
        }
        // Like `hdlsim run`: exit 1 if any test failed.
        let failures = session.sim.as_ref().map(|s| s.failures()).unwrap_or(0);
        Ok(if failures > 0 { 1 } else { 0 })
    }
}

struct DoSession {
    mgr: LibraryManager,
    files: Vec<PathBuf>,
    top: String,
    max_time_fs: u64,
    sim: Option<Simulator>,
    vcd_path: Option<PathBuf>,
}

impl DoSession {
    fn build_sim(&mut self) -> Result<(), String> {
        let mut design = Design::default();
        self.mgr.compile_files(&mut design, &self.files);
        if self.mgr.diags().has_errors() {
            return Err(self.mgr.diags().render_all(self.mgr.db()));
        }
        let flat = elab::elaborate_with(&mut design, &self.top, &mut self.mgr)
            .map_err(|e| format!("elaboration error: {}", e))?;
        if self.mgr.diags().has_errors() {
            return Err(self.mgr.diags().render_all(self.mgr.db()));
        }
        let mut sim = Simulator::new(flat);
        sim.add_observer(Box::new(Transcript));
        if let Some(p) = &self.vcd_path {
            attach_vcd(&mut sim, p)?;
        }
        self.sim = Some(sim);
        Ok(())
    }

    fn command(&mut self, args: &[String]) -> Result<Flow, String> {
        match args.first().map(|s| s.as_str()) {
            Some("run") => {
                if self.sim.is_none() {
                    self.build_sim()?;
                }
                let sim = self.sim.as_mut().unwrap();
                match args.get(1).map(|s| s.as_str()) {
                    None | Some("-all") => sim.run_until(self.max_time_fs),
                    Some(t) => {
                        let d = parse_time(t)?;
                        let target = sim.time().saturating_add(d);
                        sim.run_until(target);
                    }
                }
                Ok(Flow::Continue)
            }
            Some("restart") => {
                self.sim = None;
                self.build_sim()?;
                Ok(Flow::Continue)
            }
            Some("vcd") if args.get(1).map(|s| s.as_str()) == Some("dumpfile") => {
                let path = args.get(2).ok_or("vcd dumpfile needs a path")?.clone();
                let p = PathBuf::from(&path);
                self.vcd_path = Some(p.clone());
                if let Some(sim) = self.sim.as_mut() {
                    attach_vcd(sim, &p)?;
                }
                Ok(Flow::Continue)
            }
            Some("examine") => {
                let name = args.get(1).ok_or("examine needs a signal name")?;
                let sim = self
                    .sim
                    .as_ref()
                    .ok_or("examine: simulation not started (use `run` first)")?;
                match sim.signal_id(name) {
                    Some(id) => println!(
                        "{} = {}",
                        name,
                        hdl_sim::debug::fmt_value(&sim.signal_value(id))
                    ),
                    None => return Err(format!("examine: no signal '{}'", name)),
                }
                Ok(Flow::Continue)
            }
            Some(other) => Err(format!("unknown do-file command '{}'", other)),
            None => Ok(Flow::Continue),
        }
    }
}

fn attach_vcd(sim: &mut Simulator, path: &std::path::Path) -> Result<(), String> {
    let file = std::fs::File::create(path)
        .map_err(|e| format!("cannot create {}: {}", path.display(), e))?;
    let sink = hdl_wave::sinks::vcd::VcdSink::new(file, sim.flat());
    sim.add_observer(Box::new(hdl_wave::SinkAdapter {
        sink,
        flat: sim.flat().clone(),
    }));
    Ok(())
}
