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

/// Printed on every non-zero exit, so users know how to report the failure:
/// pack a reproducer bundle, then open an issue with the error output.
fn report_hint() {
    eprintln!(
        "note: hdlsim {} — to report this, attach a reproducer and open an issue:",
        env!("CARGO_PKG_VERSION")
    );
    eprintln!("        hdlsim bundle --out hdlsim-repro.tar --top <top> [files...]");
    eprintln!("      https://github.com/larsjoost/hdlsim/issues/new");
    eprintln!(
        "        or rerun with `--report \"what went wrong\"` to file the issue automatically"
    );
}

fn main() {
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(log::LevelFilter::Info);

    let args: Vec<String> = std::env::args().skip(1).collect();
    let (args, color) = hdlsim::commands::apply_color_args(&args);
    if let Some(mode) = color {
        hdl_syntax::diagnostic::set_color_mode(mode);
    }
    // Global `--report <msg>` / `--report-dry-run`: on failure, file a GitHub
    // issue with a reproducer bundle instead of only printing the URL.
    let (args, report) = hdlsim::commands::report::extract_report_args(&args);
    if report.is_some() {
        // Capture the rendered diagnostics so the filed issue includes the
        // exact error/warning output, not just the reproducer bundle.
        hdl_syntax::diagnostic::set_capture(true);
    }
    match hdlsim::commands::dispatch(&args) {
        Ok(code) => {
            if code != 0 {
                report_hint();
                if let Some(req) = &report {
                    hdlsim::commands::report::maybe_file_issue(req, &args);
                }
            }
            std::process::exit(code)
        }
        Err(e) => {
            eprintln!("hdlsim: {:#}", e);
            eprintln!("try 'hdlsim --help'");
            report_hint();
            if let Some(req) = &report {
                hdlsim::commands::report::maybe_file_issue(req, &args);
            }
            std::process::exit(2);
        }
    }
}
