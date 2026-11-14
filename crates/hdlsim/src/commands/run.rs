//! `hdlsim run` — compile (with libraries) and simulate one design.

use super::{build_manager, extract_common, parse_time, Command};
use hdl_ir::elab;
use hdl_ir::Design;
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

pub struct Run;

impl Command for Run {
    fn name(&self) -> &'static str {
        "run"
    }
    fn about(&self) -> &'static str {
        "compile and simulate: hdlsim run --top tb [files] [--vcd out.vcd] [--max-time 10us]"
    }
    fn run(&self, args: &[String]) -> Result<i32, String> {
        let (opts, rest) = extract_common(args)?;
        let mut files: Vec<PathBuf> = Vec::new();
        let mut top: Option<String> = None;
        let mut vcd: Option<PathBuf> = None;
        let mut max_time_fs = u64::MAX;
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
        if files.is_empty() {
            return Err("no input files".to_string());
        }
        // Compile the explicit files, then let elaboration pull in any missing
        // design units from the configured libraries on demand.
        let mut mgr = build_manager(&opts)?;
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
        let mut sim = Simulator::new(flat);
        sim.add_observer(Box::new(Transcript));
        if let Some(path) = vcd {
            let file = std::fs::File::create(&path)
                .map_err(|e| format!("cannot create {}: {}", path.display(), e))?;
            let sink = hdl_wave::sinks::vcd::VcdSink::new(file, sim.flat());
            sim.add_observer(Box::new(hdl_wave::SinkAdapter {
                sink,
                flat: sim.flat().clone(),
            }));
        }
        sim.run(max_time_fs);
        println!(
            "hdlsim: simulation stopped at t={} fs ({} signals, {} processes)",
            sim.time(),
            sim.flat().signals.len(),
            sim.flat().procs.len()
        );
        Ok(0)
    }
}
