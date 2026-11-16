//! `hdlsim wave` — render a wave file (`.hwf` from `run --hwf`, or `.fst` from
//! `run --fst` / any GTKWave-produced FST file) into the self-contained HTML
//! viewer, without re-running the simulation:
//! `hdlsim wave waves.hwf [-o out.html]`. Default output: same name, `.html`.

use super::{parse_with, Command};
use clap::Parser;
use hdl_wave::sinks::WaveSink;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    about = "render a wave file (.hwf/.fst) into the HTML viewer: hdlsim wave <file> [-o out.html]"
)]
struct WaveArgs {
    /// Input wave file (.hwf or .fst).
    input: PathBuf,
    /// Output HTML file (default: input name with .html extension).
    #[arg(long, short = 'o')]
    out: Option<PathBuf>,
}

pub struct Wave;

impl Command for Wave {
    fn name(&self) -> &'static str {
        "wave"
    }
    fn about(&self) -> &'static str {
        "render a .hwf/.fst wave file to the HTML viewer: hdlsim wave file.hwf [-o out.html]"
    }
    fn run(&self, args: &[String]) -> Result<i32, String> {
        let a = match parse_with::<WaveArgs>("wave", args) {
            Ok(a) => a,
            Err(code) => return Ok(code),
        };
        let input = &a.input;
        let out = match &a.out {
            Some(p) => p.clone(),
            None => input.with_extension("html"),
        };
        let bytes =
            std::fs::read(&input).map_err(|e| format!("cannot read {}: {}", input.display(), e))?;
        let (flat, events) = match input.extension().and_then(|e| e.to_str()) {
            Some("fst") => hdl_wave::fst::read_fst(&bytes)?,
            _ => hdl_wave::sinks::hwf::read_hwf(&bytes)?,
        };
        let file = std::fs::File::create(&out)
            .map_err(|e| format!("cannot create {}: {}", out.display(), e))?;
        let mut sink = hdl_wave::sinks::html::HtmlSink::new(file, out.clone(), &flat);
        for ev in &events {
            sink.on_event(ev, &flat);
        }
        sink.finish(&flat);
        println!(
            "wrote {} ({} signals, {} events)",
            out.display(),
            flat.signals.len(),
            events.len()
        );
        Ok(0)
    }
}
