//! Peak-RSS reading and the `HDLSIM_MAX_MEM_MB` limit: a run that would
//! exhaust memory must stop with an actionable diagnostic instead of being
//! OOM-killed — an AI debugging a design needs the message, not a lost
//! process (exit 137, no output).

/// Peak resident set size in KiB (Linux `/proc/self/status` `VmHWM`).
/// `None` when unavailable (non-Linux): callers then skip the check.
pub fn peak_rss_kb() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

/// Configured memory limit in KiB (`HDLSIM_MAX_MEM_MB`, in MiB). Unset or
/// zero disables the guard.
pub fn mem_limit_kb() -> Option<u64> {
    std::env::var("HDLSIM_MAX_MEM_MB")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .map(|mb| mb * 1024)
}

/// The actionable over-limit diagnostic. `phase` names where the run was
/// (compile / elaborate / simulate / sim-init).
pub fn limit_message(phase: &str, peak_kb: u64, limit_kb: u64) -> String {
    format!(
        "error: memory limit exceeded (peak {} MB > HDLSIM_MAX_MEM_MB={} MB) during {} — \
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
}
