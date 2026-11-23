//! Peak-RSS reading and the memory guards: a run that would exhaust
//! memory must stop with an actionable diagnostic instead of being
//! OOM-killed — an AI debugging a design needs the message, not a lost
//! process (exit 137, no output).
//!
//! The limits are armed explicitly by the CLI from `--max-mem-mb` /
//! `--max-signal-mb` (environment variables have no spell check, so a
//! typo'd limit silently guarded nothing). Library users call
//! [`set_limits`] the same way. The stored values are read from free
//! functions in the sim kernel and the CLI's compile phases, which have
//! no common state to thread them through — hence the process-global
//! cell, matching the old env contract's "set once per process".

use std::sync::atomic::{AtomicU64, Ordering};

static MEM_LIMIT_MB: AtomicU64 = AtomicU64::new(0);
static SIGNAL_LIMIT_MB: AtomicU64 = AtomicU64::new(4096);

/// Arm the guards: `mem_mb` — total peak-RSS limit (`None`/0 disables);
/// `signal_mb` — per-signal allocation cap (0 restores the 4096 default).
pub fn set_limits(mem_mb: Option<u64>, signal_mb: u64) {
    MEM_LIMIT_MB.store(mem_mb.unwrap_or(0), Ordering::Relaxed);
    SIGNAL_LIMIT_MB.store(
        if signal_mb == 0 { 4096 } else { signal_mb },
        Ordering::Relaxed,
    );
}

/// Peak resident set size in KiB (Linux `/proc/self/status` `VmHWM`).
/// `None` when unavailable (non-Linux): callers then skip the check.
pub fn peak_rss_kb() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

/// Configured total memory limit in KiB (`--max-mem-mb`, in MiB).
/// `None` when unset or zero: the guard is disabled.
pub fn mem_limit_kb() -> Option<u64> {
    let mb = MEM_LIMIT_MB.load(Ordering::Relaxed);
    (mb > 0).then(|| mb * 1024)
}

/// Configured per-signal allocation cap in bytes (`--max-signal-mb`,
/// default 4096 MiB).
pub fn signal_limit_bytes() -> u64 {
    SIGNAL_LIMIT_MB.load(Ordering::Relaxed) * 1024 * 1024
}

/// The actionable over-limit diagnostic. `phase` names where the run was
/// (compile / elaborate / simulate / sim-init).
pub fn limit_message(phase: &str, peak_kb: u64, limit_kb: u64) -> String {
    format!(
        "error: memory limit exceeded (peak {} MB > --max-mem-mb {} MB) during {} — \
         stopping before the OS OOM-killer intervenes; mitigate by reducing the design \
         (narrower generics, fewer instances), raising the limit, or selecting fewer \
         signals for wave/state capture",
        peak_kb / 1024,
        limit_kb / 1024,
        phase
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peak_rss_is_reported_on_linux() {
        match peak_rss_kb() {
            Some(kb) => assert!(kb > 0, "VmHWM should be positive, got {kb}"),
            None => eprintln!("non-Linux or /proc unavailable — check skipped"),
        }
    }

    #[test]
    fn limits_are_armed_explicitly() {
        set_limits(Some(1024), 0);
        assert_eq!(mem_limit_kb(), Some(1024 * 1024));
        assert_eq!(signal_limit_bytes(), 4096 * 1024 * 1024); // 0 → default
        set_limits(None, 64);
        assert_eq!(mem_limit_kb(), None);
        assert_eq!(signal_limit_bytes(), 64 * 1024 * 1024);
        set_limits(None, 4096); // restore the default for other tests
    }
}
