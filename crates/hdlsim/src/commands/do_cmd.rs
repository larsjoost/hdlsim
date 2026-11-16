//! `hdlsim do` — run a mini-Tcl do-file against a simulated design
//! (phase 2 of docs/plan.md).
//!
//! Usage: `hdlsim do script.do --top tb [--max-time 10us] [files...]`
//! (plus the common `-L`/`--config`/`-I` flags). The Tcl subset lives in
//! `hdlsim::tcl`; simulator commands:
//! - `run [time|-all]` — advance the sim (incremental; `run 100ns` steps)
//! - `restart` — rebuild the simulation from scratch
//! - `back [N]` — reverse debug: undo the last N `run` steps (in-memory checkpoints)
//! - `vcd dumpfile PATH` — attach a VCD sink from this point on
//! - `examine NAME` — print a signal's current value
//! - `seed N` — set the `$random` RNG seed (reproducible stimulus)
//! - `quit`/`exit [code]` — stop the script (Tcl builtin)

use super::run::Transcript;
use super::{
    build_manager, parse_time, parse_with, remap_into_bundle, Command, CommonArgs, CommonOpts,
};
use crate::tcl::{Flow, Interp};
use clap::Parser;
use hdl_ir::elab;
use hdl_ir::Design;
use hdl_sim::Simulator;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    about = "run a mini-Tcl do-file: hdlsim do <script.do> --top <tb> [--max-time 10us] [files...]"
)]
struct DoArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// Do-file script.
    script: PathBuf,
    /// Top-level unit.
    #[arg(long)]
    top: Option<String>,
    /// Stop time (e.g. 10us).
    #[arg(long)]
    max_time: Option<String>,
    /// Source files.
    files: Vec<PathBuf>,
}

pub struct DoCmd;

impl Command for DoCmd {
    fn name(&self) -> &'static str {
        "do"
    }
    fn about(&self) -> &'static str {
        "run a mini-Tcl do-file: hdlsim do script.do --top tb [files]"
    }
    fn run(&self, args: &[String]) -> anyhow::Result<i32> {
        let a = match parse_with::<DoArgs>("do", args) {
            Ok(a) => a,
            Err(code) => return Ok(code),
        };
        let opts = a.common.into_opts()?;
        let script = a.script;
        let top = a
            .top
            .ok_or("--top is required")
            .map_err(anyhow::Error::msg)?;
        let max_time_fs = match &a.max_time {
            Some(v) => parse_time(v)?,
            None => u64::MAX,
        };
        let files: Vec<PathBuf> = a.files;
        let text = std::fs::read_to_string(&script)
            .map_err(|e| anyhow::anyhow!("cannot read {}: {}", script.display(), e))?;
        let (_mgr, root) = build_manager(&opts)?;
        let files: Vec<PathBuf> = files.iter().map(|f| remap_into_bundle(f, &root)).collect();

        let mut session = DoSession {
            opts,
            files,
            top,
            max_time_fs,
            sim: None,
            vcd_path: None,
            history: Vec::new(),
            seed: None,
        };
        let mut interp = Interp::new();
        let flow = interp
            .eval(&text, &mut |_i, args| {
                session.command(args).map_err(|e| e.to_string())
            })
            .map_err(anyhow::Error::msg)?;
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
    /// Kept so `restart` can build a fresh LibraryManager — the compiled-file
    /// set of a used manager would skip the files and leave the new design empty.
    opts: CommonOpts,
    files: Vec<PathBuf>,
    top: String,
    max_time_fs: u64,
    sim: Option<Simulator>,
    vcd_path: Option<PathBuf>,
    /// Reverse-debug history: (time_fs, checkpoint bytes) pushed after each
    /// `run`; `back` pops and restores.
    history: Vec<(u64, Vec<u8>)>,
    /// `$random` seed from a `seed N` command; applied to (re)built sims.
    seed: Option<u64>,
}

impl DoSession {
    fn build_sim(&mut self) -> anyhow::Result<()> {
        let (mut mgr, _root) = build_manager(&self.opts).map_err(anyhow::Error::msg)?;
        let mut design = Design::default();
        mgr.compile_files(&mut design, &self.files);
        if mgr.diags().has_errors() {
            return Err(anyhow::Error::msg(
                mgr.diags().render_all(mgr.db()).trim_end().to_string(),
            ));
        }
        let flat = elab::elaborate_with(&mut design, &self.top, &mut mgr)
            .map_err(|e| anyhow::anyhow!("elaboration error: {}", e))
            .map_err(anyhow::Error::msg)?;
        if mgr.diags().has_errors() {
            return Err(anyhow::Error::msg(
                mgr.diags().render_all(mgr.db()).trim_end().to_string(),
            ));
        }
        let mut sim = Simulator::new(flat);
        if let Some(s) = self.seed {
            sim.set_seed(s);
        }
        sim.add_observer(Box::new(Transcript));
        if let Some(p) = &self.vcd_path {
            attach_vcd(&mut sim, p)?;
        }
        // Seed the reverse-debug history with the t=0 state.
        let bytes = sim.save_checkpoint_bytes();
        self.history.push((0, bytes));
        self.sim = Some(sim);
        Ok(())
    }

    fn command(&mut self, args: &[String]) -> anyhow::Result<Flow> {
        match args.first().map(|s| s.as_str()) {
            Some("run") => {
                if self.sim.is_none() {
                    self.build_sim()?;
                }
                let (t, bytes) = {
                    let sim = self.sim.as_mut().unwrap();
                    match args.get(1).map(|s| s.as_str()) {
                        None | Some("-all") => sim.run_until(self.max_time_fs),
                        Some(t) => {
                            let d = parse_time(t).map_err(anyhow::Error::msg)?;
                            let target = sim.time().saturating_add(d);
                            sim.run_until(target);
                        }
                    }
                    (sim.time(), sim.save_checkpoint_bytes())
                };
                self.history.push((t, bytes));
                Ok(Flow::Continue)
            }
            Some("back") => {
                // Reverse debug: undo the last `run` step (or N steps).
                let n = match args.get(1).map(|s| s.as_str()) {
                    None => 1usize,
                    Some(t) => t
                        .parse::<usize>()
                        .map_err(|_| anyhow::anyhow!("back: bad step count '{}'", t))?,
                };
                if n == 0 || n >= self.history.len() {
                    return Err(anyhow::anyhow!(
                        "back: only {} step(s) of history",
                        self.history.len().saturating_sub(1)
                    ));
                }
                self.history.truncate(self.history.len() - n);
                let (t, bytes) = self.history.last().unwrap().clone();
                let flat = match &self.sim {
                    Some(s) => s.flat().clone(),
                    None => {
                        return Err(anyhow::anyhow!(
                            "back: simulation not started (use `run` first)"
                        ))
                    }
                };
                let mut sim = Simulator::restore_checkpoint_bytes(flat, &bytes)
                    .map_err(anyhow::Error::msg)?;
                sim.add_observer(Box::new(Transcript));
                if let Some(p) = &self.vcd_path {
                    attach_vcd(&mut sim, p)?;
                }
                self.sim = Some(sim);
                println!("hdlsim do: back to t={}", hdl_sim::debug::fmt_time(t));
                Ok(Flow::Continue)
            }
            Some("restart") => {
                self.sim = None;
                self.history.clear();
                self.build_sim()?;
                Ok(Flow::Continue)
            }
            Some("vcd") if args.get(1).map(|s| s.as_str()) == Some("dumpfile") => {
                let path = args
                    .get(2)
                    .ok_or("vcd dumpfile needs a path")
                    .map_err(anyhow::Error::msg)?
                    .clone();
                let p = PathBuf::from(&path);
                self.vcd_path = Some(p.clone());
                if let Some(sim) = self.sim.as_mut() {
                    attach_vcd(sim, &p)?;
                }
                Ok(Flow::Continue)
            }
            Some("seed") => {
                let s: u64 = args
                    .get(1)
                    .ok_or("seed needs a u64 value")
                    .map_err(anyhow::Error::msg)?
                    .parse()
                    .map_err(|_| anyhow::anyhow!("seed needs a u64 value"))?;
                self.seed = Some(s);
                if let Some(sim) = self.sim.as_ref() {
                    sim.set_seed(s);
                }
                println!("hdlsim do: seed = {}", s);
                Ok(Flow::Continue)
            }
            Some("examine") => {
                let name = args
                    .get(1)
                    .ok_or("examine needs a signal name")
                    .map_err(anyhow::Error::msg)?;
                let sim = self
                    .sim
                    .as_ref()
                    .ok_or("examine: simulation not started (use `run` first)")
                    .map_err(anyhow::Error::msg)?;
                match sim.signal_id(name) {
                    Some(id) => println!(
                        "{} = {}",
                        name,
                        hdl_sim::debug::fmt_value(&sim.signal_value(id))
                    ),
                    None => return Err(anyhow::anyhow!("examine: no signal '{}'", name)),
                }
                Ok(Flow::Continue)
            }
            Some("checkpoint") => {
                let path = args
                    .get(1)
                    .ok_or("checkpoint needs a path")
                    .map_err(anyhow::Error::msg)?;
                let sim = self
                    .sim
                    .as_mut()
                    .ok_or("checkpoint: simulation not started (use `run` first)")
                    .map_err(anyhow::Error::msg)?;
                sim.save_checkpoint(std::path::Path::new(path))
                    .map_err(|e| anyhow::anyhow!("checkpoint: {}", e))
                    .map_err(anyhow::Error::msg)?;
                println!("hdlsim do: checkpoint written to {}", path);
                Ok(Flow::Continue)
            }
            Some("restore") => {
                let path = args
                    .get(1)
                    .ok_or("restore needs a path")
                    .map_err(anyhow::Error::msg)?;
                if self.sim.is_none() {
                    self.build_sim()?;
                }
                let flat = self.sim.as_ref().unwrap().flat().clone();
                let mut sim = Simulator::restore_checkpoint(flat, std::path::Path::new(path))
                    .map_err(anyhow::Error::msg)?;
                sim.add_observer(Box::new(Transcript));
                if let Some(p) = &self.vcd_path {
                    attach_vcd(&mut sim, p)?;
                }
                self.sim = Some(sim);
                self.history.clear();
                println!("hdlsim do: restored checkpoint {}", path);
                Ok(Flow::Continue)
            }
            Some(other) => Err(anyhow::anyhow!("unknown do-file command '{}'", other)),
            None => Ok(Flow::Continue),
        }
    }
}

fn attach_vcd(sim: &mut Simulator, path: &std::path::Path) -> anyhow::Result<()> {
    let file = std::fs::File::create(path)
        .map_err(|e| anyhow::anyhow!("cannot create {}: {}", path.display(), e))
        .map_err(anyhow::Error::msg)?;
    let sink = hdl_wave::sinks::vcd::VcdSink::new(file, sim.flat());
    sim.add_observer(Box::new(hdl_wave::SinkAdapter {
        sink,
        flat: sim.flat().clone(),
    }));
    Ok(())
}
