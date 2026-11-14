//! `hdlsim init` — create a starter `hdlsim.ini` library config in the cwd.

use super::Command;
use std::path::Path;

const STARTER: &str = "# hdlsim library configuration (modelsim.ini style)\n[Library]\nwork = .\n";

pub struct Init;

impl Command for Init {
    fn name(&self) -> &'static str {
        "init"
    }
    fn about(&self) -> &'static str {
        "create a starter hdlsim.ini library config in the current directory"
    }
    fn run(&self, args: &[String]) -> Result<i32, String> {
        let force = args.iter().any(|a| a == "--force");
        let path = Path::new("hdlsim.ini");
        if path.exists() && !force {
            return Err("hdlsim.ini already exists (use --force to overwrite)".to_string());
        }
        std::fs::write(path, STARTER).map_err(|e| format!("cannot write hdlsim.ini: {}", e))?;
        println!("created hdlsim.ini — add libraries like:  mylib = path/to/lib");
        Ok(0)
    }
}
