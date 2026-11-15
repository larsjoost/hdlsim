//! `hdlsim doctor` - environment sanity report: stdlib location and unit
//! resolution, project config, `-L`/`-I` dirs, parse-cache writability.
//! Prints one line per check and exits 1 if any check fails.

use super::{extract_common, Command};
use hdl_lib::doctor::{cache_from_env, checks, Status};
use std::path::PathBuf;

pub struct Doctor;

impl Command for Doctor {
    fn name(&self) -> &'static str {
        "doctor"
    }
    fn about(&self) -> &'static str {
        "check the environment (stdlib, config, libraries, cache): hdlsim doctor"
    }
    fn run(&self, args: &[String]) -> Result<i32, String> {
        let (opts, rest) = extract_common(args)?;
        if !rest.is_empty() {
            return Err(format!("doctor takes no arguments (got '{}')", rest[0]));
        }
        let config = match &opts.config {
            Some(c) => Some(c.clone()),
            None => {
                let default = PathBuf::from("hdlsim.ini");
                default.is_file().then_some(default)
            }
        };
        let cs = checks(
            config.as_deref(),
            &opts.libs,
            &opts.incdirs,
            cache_from_env(),
        );
        let mut fails = 0usize;
        let mut warns = 0usize;
        println!(
            "hdlsim {} \u{2014} environment check",
            env!("CARGO_PKG_VERSION")
        );
        for c in &cs {
            let (tag, is_fail, is_warn) = match c.status {
                Status::Ok => ("ok  ", false, false),
                Status::Warn => ("warn", false, true),
                Status::Fail => ("FAIL", true, false),
            };
            if is_fail {
                fails += 1;
            }
            if is_warn {
                warns += 1;
            }
            println!("{} {:<13} {}", tag, c.name, c.detail);
        }
        println!(
            "doctor: {} ok, {} warn, {} fail",
            cs.len() - fails - warns,
            warns,
            fails
        );
        Ok(if fails > 0 { 1 } else { 0 })
    }
}
