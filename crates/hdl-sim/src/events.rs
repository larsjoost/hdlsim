//! Simulation events and the observer bus — the extension seam of the kernel.
//!
//! The kernel broadcasts a closed [`SimEvent`] enum to all registered
//! [`Observer`]s (wave sinks, coverage, live server, …). New analysis features
//! are new observers — the kernel never changes for them.
//!
//! Invariant: events are immutable snapshots; observers cannot corrupt the
//! kernel through them.

use hdl_ir::Value;

#[derive(Clone, Debug)]
pub enum SimEvent {
    /// Simulation time advanced to `time_fs`.
    TimeAdvance { time_fs: u64 },
    /// A signal changed value at `time_fs`.
    SignalChange {
        sig: u32,
        old: Value,
        new: Value,
        time_fs: u64,
    },
    /// A process started (`true`) / finished (`false`) executing.
    ProcActive {
        proc: u32,
        active: bool,
        time_fs: u64,
    },
    /// A `$display`/`$write`/`report` line of text.
    Message { text: String, time_fs: u64 },
    /// `$finish` / `std.env.finish` was called.
    Finish { time_fs: u64 },
    /// Simulation stopped (end of events, no $finish).
    Stop { time_fs: u64 },
}

/// Extension seam: implement and register with
/// [`crate::Simulator::add_observer`]. See `docs/extensibility.md`.
pub trait Observer {
    fn on_event(&mut self, ev: &SimEvent);
    /// Called once after `Simulator::run` returns — sinks flush output here.
    fn on_finish(&mut self) {}
}

/// A no-op observer, useful as a base and in tests.
pub struct NullObserver;
impl Observer for NullObserver {
    fn on_event(&mut self, _ev: &SimEvent) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Counter(u32);
    impl Observer for Counter {
        fn on_event(&mut self, ev: &SimEvent) {
            if matches!(ev, SimEvent::SignalChange { .. }) {
                self.0 += 1;
            }
        }
    }

    #[test]
    fn observers_receive_events() {
        let mut c = Counter(0);
        c.on_event(&SimEvent::SignalChange {
            sig: 0,
            old: Value::from_u64(1, 0),
            new: Value::from_u64(1, 1),
            time_fs: 0,
        });
        assert_eq!(c.0, 1);
    }
}
