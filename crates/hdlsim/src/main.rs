//! Binary entry point — all logic lives in the library (`lib.rs`).

/// Minimal stderr logger so the library crates can trace via the `log` facade
/// (e.g. `hdl-wave` write failures, `hdl-sim` live watch echoes).
struct StderrLogger;

impl log::Log for StderrLogger {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        true
    }
    fn log(&self, record: &log::Record<'_>) {
        eprintln!("{}: {}", record.level(), record.args());
    }
    fn flush(&self) {}
}

static LOGGER: StderrLogger = StderrLogger;

fn main() {
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(log::LevelFilter::Info);

    let args: Vec<String> = std::env::args().skip(1).collect();
    let (args, color) = hdlsim::commands::apply_color_args(&args);
    if let Some(mode) = color {
        hdl_syntax::diagnostic::set_color_mode(mode);
    }
    match hdlsim::commands::dispatch(&args) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("hdlsim: {:#}", e);
            eprintln!("try 'hdlsim --help'");
            std::process::exit(2);
        }
    }
}
