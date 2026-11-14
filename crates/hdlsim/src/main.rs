//! # hdlsim
//!
//! The command-line interface. This crate is intentionally thin: argument parsing
//! and I/O only — all logic lives in the library crates (`hdl-syntax`, `hdl-lib`,
//! `hdl-ir`, `hdl-sim`, `hdl-wave`).
//!
//! Extension seam: `trait Command` registered in `commands` — a new subcommand is
//! a new module plus one registration line. See `docs/extensibility.md`.

mod commands;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match commands::dispatch(&args) {
        Ok(code) => std::process::exit(code),
        Err(msg) => {
            eprintln!("hdlsim: {}", msg);
            eprintln!("try 'hdlsim --help'");
            std::process::exit(2);
        }
    }
}
