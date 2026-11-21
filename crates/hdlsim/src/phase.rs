//! Phase timing and peak-RSS reporting: one stderr line per run phase
//! (compile / elaborate / simulate). A run localizes its own time and
//! memory so debugging needs no external `ps` sampling — which is easy to
//! aim at the wrong process when the command is wrapped in a shell.

use std::fmt;
use std::time::Instant;

/// One timed phase. Print with [`Phase::done`].
pub struct Phase {
    name: &'static str,
    start: Instant,
}

impl Phase {
    pub fn start(name: &'static str) -> Phase {
        Phase {
            name,
            start: Instant::now(),
        }
    }

    /// Close the phase: `hdlsim: <name> <secs>s (<detail>) [peak-rss N MB]`.
    pub fn done(self, detail: impl fmt::Display) {
        let secs = self.start.elapsed().as_secs_f32();
        let rss = match hdl_sim::meminfo::peak_rss_kb() {
            Some(kb) => format!(" peak-rss {} MB", kb / 1024),
            None => String::new(),
        };
        eprintln!("hdlsim: {} {:.2}s ({}){}", self.name, secs, detail, rss);
    }
}

/// CLI-side memory-limit check at a phase boundary (compile/elaborate happen
/// before the simulator exists, so its in-loop guard cannot see them):
/// prints the actionable diagnostic and returns true when over the limit.
pub fn over_mem_limit(phase: &str) -> bool {
    let Some(limit_kb) = hdl_sim::meminfo::mem_limit_kb() else {
        return false;
    };
    let Some(peak_kb) = hdl_sim::meminfo::peak_rss_kb() else {
        return false;
    };
    if peak_kb <= limit_kb {
        return false;
    }
    eprintln!(
        "hdlsim: {}",
        hdl_sim::meminfo::limit_message(phase, peak_kb, limit_kb)
    );
    true
}
