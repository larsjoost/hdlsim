//! Subcommand registry — the CLI extension seam.
//!
//! Each subcommand is a module implementing [`Command`] and registered in
//! [`COMMANDS`]. `dispatch` looks the first argument up in the registry.

use crate::compile;
use hdl_ir::elab;
use hdl_sim::{Observer, SimEvent, Simulator};
use std::path::PathBuf;

/// Extension seam: implement for a new subcommand, add one entry to [`COMMANDS`].
pub trait Command {
    fn name(&self) -> &'static str;
    /// Human-readable one-liner for `hdlsim --help`.
    fn about(&self) -> &'static str;
    /// Run with the arguments after the subcommand name. Returns the exit code.
    fn run(&self, args: &[String]) -> Result<i32, String>;
}

struct Help;

impl Command for Help {
    fn name(&self) -> &'static str {
        "--help"
    }
    fn about(&self) -> &'static str {
        "show this help"
    }
    fn run(&self, _args: &[String]) -> Result<i32, String> {
        println!("hdlsim 0.1.0 — self-contained VHDL + Verilog simulator");
        println!("usage: hdlsim <command> [args]");
        for c in COMMANDS {
            println!("  {:<12} {}", c.name(), c.about());
        }
        Ok(0)
    }
}

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

struct Run;

impl Command for Run {
    fn name(&self) -> &'static str {
        "run"
    }
    fn about(&self) -> &'static str {
        "compile and simulate: hdlsim run --top tb [files] [--vcd out.vcd] [--max-time 10us]"
    }
    fn run(&self, args: &[String]) -> Result<i32, String> {
        let mut files: Vec<PathBuf> = Vec::new();
        let mut top: Option<String> = None;
        let mut vcd: Option<PathBuf> = None;
        let mut max_time_fs = u64::MAX;
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--top" => {
                    top = Some(args.get(i + 1).ok_or("--top needs a value")?.clone());
                    i += 2;
                }
                "--vcd" => {
                    vcd = Some(PathBuf::from(args.get(i + 1).ok_or("--vcd needs a value")?));
                    i += 2;
                }
                "--max-time" => {
                    let v = args.get(i + 1).ok_or("--max-time needs a value")?;
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
        let compiled = compile::compile_paths(&files);
        if compiled.has_errors() {
            eprint!("{}", compiled.render_diags());
            return Ok(1);
        }
        if !compiled.diags.is_empty() {
            eprint!("{}", compiled.render_diags());
        }
        let top_name = match top {
            Some(t) => t,
            None => {
                if compiled.design.modules.len() == 1 {
                    compiled.design.modules[0].name.clone()
                } else {
                    let names: Vec<&str> = compiled
                        .design
                        .modules
                        .iter()
                        .map(|m| m.name.as_str())
                        .collect();
                    return Err(format!(
                        "multiple modules ({}); pick one with --top",
                        names.join(", ")
                    ));
                }
            }
        };
        let flat = elab::elaborate(&compiled.design, &top_name)
            .map_err(|e| format!("elaboration error: {}", e))?;
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

/// Parse a time like `100ns`, `10us`, `1ms`, `500` (bare = ns) into femtoseconds.
pub fn parse_time(s: &str) -> Result<u64, String> {
    let (num, unit) = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .map(|i| (&s[..i], &s[i..]))
        .unwrap_or((s, ""));
    let v: f64 = num.parse().map_err(|_| format!("bad time value '{}'", s))?;
    let mult: f64 = match unit {
        "" | "ns" => 1e6,
        "fs" => 1.0,
        "ps" => 1e3,
        "us" | "µs" => 1e9,
        "ms" => 1e12,
        "s" => 1e15,
        _ => return Err(format!("unknown time unit '{}'", unit)),
    };
    Ok((v * mult) as u64)
}

/// Registry of all subcommands. Add one line per new command.
pub const COMMANDS: &[&dyn Command] = &[&Help, &Run];

/// Dispatch `args` (without the binary name) to a registered command.
pub fn dispatch(args: &[String]) -> Result<i32, String> {
    match args.first() {
        None => COMMANDS[0].run(&[]),
        Some(name) => {
            let cmd = COMMANDS
                .iter()
                .find(|c| c.name() == name.as_str())
                .ok_or_else(|| format!("unknown command '{}'", name))?;
            cmd.run(&args[1..])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_lists_commands() {
        assert_eq!(dispatch(&["--help".to_string()]).unwrap(), 0);
    }

    #[test]
    fn unknown_command_is_an_error() {
        assert!(dispatch(&["frobnicate".to_string()]).is_err());
    }

    #[test]
    fn parses_times() {
        // Times are femtoseconds: 1 fs = 1, 1 ps = 1e3, 1 ns = 1e6, …
        assert_eq!(parse_time("100ns").unwrap(), 100_000_000);
        assert_eq!(parse_time("10us").unwrap(), 10_000_000_000);
        assert_eq!(parse_time("500").unwrap(), 500_000_000);
        assert_eq!(parse_time("2ms").unwrap(), 2_000_000_000_000);
        assert_eq!(parse_time("3ps").unwrap(), 3_000);
        assert!(parse_time("10xs").is_err());
    }
}
