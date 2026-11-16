//! `hdlsim init` — create a starter `hdlsim.ini` library config in the cwd.

use super::{parse_with, Command};
use clap::Parser;
use std::path::Path;

const STARTER: &str = "# hdlsim library configuration (modelsim.ini style)\n[Library]\nwork = .\n";

#[derive(Parser)]
#[command(about = "create a starter hdlsim.ini library config in the current directory")]
struct InitArgs {
    /// Overwrite an existing hdlsim.ini.
    #[arg(long)]
    force: bool,
}

pub struct Init;

impl Command for Init {
    fn name(&self) -> &'static str {
        "init"
    }
    fn about(&self) -> &'static str {
        "create a starter hdlsim.ini library config in the current directory"
    }
    fn run(&self, args: &[String]) -> anyhow::Result<i32> {
        let a = match parse_with::<InitArgs>("init", args) {
            Ok(a) => a,
            Err(code) => return Ok(code),
        };
        let path = Path::new("hdlsim.ini");
        if path.exists() && !a.force {
            return Err(anyhow::anyhow!(
                "hdlsim.ini already exists (use --force to overwrite)"
            ));
        }
        std::fs::write(path, STARTER)
            .map_err(|e| anyhow::anyhow!("cannot write hdlsim.ini: {}", e))?;
        println!("created hdlsim.ini — add libraries like:  mylib = path/to/lib");
        Ok(0)
    }
}
