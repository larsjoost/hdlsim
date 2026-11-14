//! Subcommand registry — the CLI extension seam.
//!
//! Each subcommand is a module implementing `Command` and registered in
//! `COMMANDS`. `dispatch` looks the first argument up in the registry.

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

/// Registry of all subcommands. Add one line per new command.
pub const COMMANDS: &[&dyn Command] = &[&Help];

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
}
