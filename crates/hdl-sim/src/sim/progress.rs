//! In-run stderr progress: the `--progress-sec` heartbeat ("still running",
//! printed when time stops advancing) and the `--progress` simulation ticks.
//!
//! Ticks (opt-in via [`Simulator::set_progress_ticks`], armed by the CLI's
//! `--progress`) give a long simulation a sense of distance-to-go instead of
//! looking hung. With `--max-time T`, one line is printed whenever simulated
//! time crosses a decile of `T` — `hdlsim: sim t=1.2us / 5us (24%)` — driven
//! by simulated time, so the line count is deterministic and testable.
//! Without `--max-time`, plain `hdlsim: sim t=1.2us` lines are rate-limited
//! by wall clock (the same >= 500 ms granularity the `--max-wall` check uses
//! at the pump top) and only fire when simulated time actually moved: a run
//! grinding inside one timestamp is the heartbeat's news, not a tick's.
//! Ticks stop with the run — `$finish`/`std.env.finish`, a guard stop, or
//! the budget itself — so no tick contradicts the stop line.

use super::Simulator;
use hdl_ir::fmt_time;

/// Minimum wall-clock spacing between two sim ticks when the run has no
/// `--max-time` (mirrors the `--max-wall` in-loop check granularity).
const WALL_TICK_SPACING: std::time::Duration = std::time::Duration::from_millis(500);

/// Tick state for the current run. `Armed` is what the CLI sets; the first
/// pump entry resolves it against the run's time budget.
#[derive(Clone, Copy, Default)]
pub(super) enum SimProgress {
    /// `--progress` not requested: every check below is one enum match.
    #[default]
    Off,
    /// Requested, not yet resolved against a run budget.
    Armed,
    /// Decile ticks against a finite `--max-time`: `next_fs` is the next
    /// decile of `total_fs` that simulated time must reach to tick.
    Deciles { total_fs: u64, next_fs: u64 },
    /// Wall-clock ticks (no `--max-time`): the last tick's instant and the
    /// simulated time it reported.
    Wall {
        last: Option<(std::time::Instant, u64)>,
    },
}

impl SimProgress {
    /// The first decile of `total_fs` strictly beyond `now_fs`: a restored
    /// checkpoint resumes with only the ticks still ahead of it.
    fn deciles_from(total_fs: u64, now_fs: u64) -> SimProgress {
        let decile = (total_fs / 10).max(1);
        SimProgress::Deciles {
            total_fs,
            next_fs: decile.saturating_mul(now_fs / decile + 1),
        }
    }
}

impl Simulator {
    /// Arm `--progress` simulation ticks (stderr lines while the sim runs;
    /// see the [`progress`](self) module docs). A Simulator field — not a
    /// process global — so it follows the (possibly restored) instance,
    /// like the other kernel-side knobs.
    pub fn set_progress_ticks(&mut self, on: bool) {
        self.progress = if on {
            SimProgress::Armed
        } else {
            SimProgress::Off
        };
    }

    /// Resolve `Armed` at a pump start. A hard run (`run`) re-derives the
    /// state from the current time — a restored checkpoint resumes
    /// mid-budget with only the ticks ahead of it — while a soft pump
    /// (`run_until` step) keeps the running state.
    pub(super) fn progress_arm(&mut self, max_time_fs: u64, soft: bool) {
        if !soft && !matches!(self.progress, SimProgress::Off) {
            self.progress = SimProgress::Armed;
        }
        if matches!(self.progress, SimProgress::Armed) {
            // `run` passes u64::MAX when --max-time is absent; that is the
            // pump's "no time limit" convention.
            self.progress = if max_time_fs == u64::MAX {
                SimProgress::Wall { last: None }
            } else {
                SimProgress::deciles_from(max_time_fs, self.time)
            };
        }
    }

    /// Decile tick after a time advance: one line per crossing. A single
    /// advance that jumps several deciles reports where the sim is now
    /// (not each boundary), so the line count stays deterministic.
    pub(super) fn progress_tick(&mut self) {
        let SimProgress::Deciles { total_fs, next_fs } = self.progress else {
            return;
        };
        if self.time < next_fs {
            return;
        }
        let decile = (total_fs / 10).max(1);
        let next_fs = decile.saturating_mul(self.time / decile + 1);
        self.progress = SimProgress::Deciles { total_fs, next_fs };
        let pct = self.time as f64 / total_fs as f64 * 100.0;
        eprintln!(
            "hdlsim: sim t={} / {} ({:.0}%)",
            fmt_time(self.time),
            fmt_time(total_fs),
            pct
        );
    }

    /// Wall-clock tick at the pump top (no `--max-time`): at most one line
    /// per [`WALL_TICK_SPACING`], and only when simulated time moved.
    pub(super) fn progress_wall_tick(&mut self) {
        let SimProgress::Wall { last } = self.progress else {
            return;
        };
        let now = std::time::Instant::now();
        if now.duration_since(self.wall_origin()) < WALL_TICK_SPACING {
            return;
        }
        let due = match last {
            Some((at, t_fs)) => t_fs != self.time && now.duration_since(at) >= WALL_TICK_SPACING,
            // A run still at t=0 has no simulated progress to report —
            // the heartbeat owns that case.
            None => self.time > 0,
        };
        if !due {
            return;
        }
        eprintln!("hdlsim: sim t={}", fmt_time(self.time));
        self.progress = SimProgress::Wall {
            last: Some((now, self.time)),
        };
    }

    /// Periodic "still running" note on stderr for runs that grind without
    /// time advance (`--progress <s>`, 0/absent = off). Shows
    /// the same location as the interrupt dump, so a hung or slow run
    /// announces where it is instead of silently burning CPU.
    /// Wall-clock origin: the first caller initializes it (sim start). Reads
    /// go through this helper because `Cell::get()` returns a *copy* —
    /// `get_or_insert` on the copy is silently discarded, which once broke
    /// `--max-wall` (the budget re-seeded a fresh `Instant` every check and
    /// never elapsed) and silenced the heartbeat.
    pub(super) fn wall_origin(&self) -> std::time::Instant {
        if self.wall_start.get().is_none() {
            self.wall_start.set(Some(std::time::Instant::now()));
        }
        self.wall_start
            .get()
            .unwrap_or_else(std::time::Instant::now)
    }

    pub(super) fn maybe_heartbeat(&self) {
        let interval = super::progress_interval();
        if interval.is_zero() {
            return;
        }
        let now = std::time::Instant::now();
        let start = self.wall_origin();
        if now.duration_since(start) < interval {
            return;
        }
        let report = self.last_heartbeat.get().unwrap_or(start);
        let advance = self.last_advance.unwrap_or(start);
        if now.duration_since(report) >= interval && now.duration_since(advance) >= interval {
            eprintln!(
                "hdlsim: still running ({:.0} s without time advance) at {}",
                now.duration_since(advance).as_secs_f64(),
                self.where_am_i()
            );
            self.last_heartbeat.set(Some(now));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SimProgress, WALL_TICK_SPACING};
    use crate::sim::Simulator;
    use hdl_ir::elab::{Flat, FlatSig};
    use hdl_ir::SigKind;
    use std::cell::Cell;
    use std::time::{Duration, Instant};

    /// The smallest simulable design: one wire, no processes.
    fn sim() -> Simulator {
        let flat = Flat {
            signals: vec![FlatSig {
                name: "s".to_string(),
                width: 1,
                kind: SigKind::Wire,
                init: None,
                mem: None,
                scope: 0,
                signed: false,
                real_ranges: Vec::new(),
            }],
            procs: Vec::new(),
            assigns: Vec::new(),
            functions: Vec::new(),
            scopes: Vec::new(),
            top_scope: 0,
        };
        Simulator::new(flat)
    }

    fn deciles(total_fs: u64, now_fs: u64) -> u64 {
        match SimProgress::deciles_from(total_fs, now_fs) {
            SimProgress::Deciles { next_fs, .. } => next_fs,
            _ => unreachable!("deciles_from always returns Deciles"),
        }
    }

    #[test]
    fn first_decile_of_a_fresh_run() {
        assert_eq!(deciles(400_000_000, 0), 40_000_000);
        assert_eq!(deciles(5_000_000_000, 0), 500_000_000);
    }

    #[test]
    fn restored_mid_budget_skips_the_deciles_behind_it() {
        // Restored exactly on a crossed decile: no re-tick for it.
        assert_eq!(deciles(1_000_000_000, 400_000_000), 500_000_000);
        // Restored at the budget itself: the next decile is beyond the
        // total, so nothing fires any more.
        assert_eq!(deciles(400_000_000, 400_000_000), 440_000_000);
    }

    #[test]
    fn a_tiny_budget_still_yields_a_threshold() {
        assert_eq!(deciles(5, 0), 1);
    }

    #[test]
    fn wall_tick_waits_for_spacing_and_time_movement() {
        let mut sim = sim();
        sim.set_progress_ticks(true);
        sim.progress_arm(u64::MAX, false);
        assert!(matches!(sim.progress, SimProgress::Wall { last: None }));
        // Anchor the run start in the past so the 500 ms origin gate has
        // already elapsed.
        sim.wall_start = Cell::new(Some(
            Instant::now() - WALL_TICK_SPACING - Duration::from_millis(50),
        ));
        // Still at t=0: nothing to report.
        sim.progress_wall_tick();
        assert!(matches!(sim.progress, SimProgress::Wall { last: None }));
        // First advance but inside the spacing since the last tick —
        // `last` is None here, so the origin gate is the only limiter and
        // the tick fires once time moved.
        sim.time = 1_000;
        sim.progress_wall_tick();
        let (at, t_fs) = match sim.progress {
            SimProgress::Wall { last: Some(v) } => v,
            _ => panic!("wall tick must record the print"),
        };
        assert_eq!(t_fs, 1_000);
        // Same timestamp again: suppressed even with spacing elapsed.
        sim.progress = SimProgress::Wall {
            last: Some((
                Instant::now() - WALL_TICK_SPACING - Duration::from_millis(50),
                t_fs,
            )),
        };
        sim.progress_wall_tick();
        let still = match sim.progress {
            SimProgress::Wall { last: Some((_, t)) } => t,
            _ => panic!("wall state must persist"),
        };
        assert_eq!(still, t_fs, "same timestamp must be suppressed");
        // Time moved and spacing elapsed: fires again.
        sim.time = 2_000;
        sim.progress = SimProgress::Wall {
            last: Some((at - WALL_TICK_SPACING - Duration::from_millis(50), t_fs)),
        };
        sim.progress_wall_tick();
        assert!(matches!(
            sim.progress,
            SimProgress::Wall {
                last: Some((_, 2_000))
            }
        ));
    }
}
