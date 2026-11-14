//! Binary entry point — all logic lives in the library (`lib.rs`).

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match hdlsim::commands::dispatch(&args) {
        Ok(code) => std::process::exit(code),
        Err(msg) => {
            eprintln!("hdlsim: {}", msg);
            eprintln!("try 'hdlsim --help'");
            std::process::exit(2);
        }
    }
}
