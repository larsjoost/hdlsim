//! Simulation events and the observer bus — the extension seam of the kernel.
//!
//! The kernel broadcasts a closed [`SimEvent`] enum to all registered
//! [`Observer`]s (wave sinks, coverage, live server, …). New analysis features
//! are new observers — the kernel never changes for them.
//!
//! Invariant: events are immutable snapshots; observers cannot corrupt the
//! kernel through them.

use hdl_ir::Value;
use hdl_syntax::source::Span;

/// Severity of a report/assert message (LRM 1076 §10.2 `severity_level`),
/// resolved by the lowerer/kernel so a transcript renderer can color the line
/// and prefix the emitting statement's location without sniffing the text.
/// `None` marks output with no VHDL severity — plain `$display`/`$write` and
/// textio echoes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MessageSeverity {
    Note,
    Warning,
    Error,
    Failure,
}

impl MessageSeverity {
    /// LRM severity_level name, lowercase.
    pub fn name(self) -> &'static str {
        match self {
            MessageSeverity::Note => "note",
            MessageSeverity::Warning => "warning",
            MessageSeverity::Error => "error",
            MessageSeverity::Failure => "failure",
        }
    }

    /// Parse a lowercase LRM severity name; `None` for anything else.
    pub fn from_name(name: &str) -> Option<MessageSeverity> {
        match name {
            "note" => Some(MessageSeverity::Note),
            "warning" => Some(MessageSeverity::Warning),
            "error" => Some(MessageSeverity::Error),
            "failure" => Some(MessageSeverity::Failure),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub enum SimEvent {
    /// A statement executed (carries its source span) — drives line/branch
    /// coverage collectors. Emitted for every `Stmt::Spanned`.
    StmtExec { span: Span, time_fs: u64 },
    /// Simulation time advanced to `time_fs`.
    TimeAdvance { time_fs: u64 },
    /// A signal changed value at `time_fs`.
    SignalChange {
        sig: u32,
        old: Value,
        new: Value,
        time_fs: u64,
    },
    /// The driver of `sig` changed (a different process, or a process vs. a
    /// continuous assign, wrote it last) — wave-viewer driver tracing.
    /// `proc == u32::MAX` means a continuous `assign`.
    Driver { sig: u32, proc: u32, time_fs: u64 },
    /// A process started (`true`) / finished (`false`) executing.
    ProcActive {
        proc: u32,
        active: bool,
        time_fs: u64,
    },
    /// A `$display`/`$write`/`report` line of text. `span` is the source
    /// statement that produced it when known, so a transcript reader (human
    /// or agent) can jump straight to the source line. `severity` is the
    /// VHDL severity level for report/assert output (`None` for plain
    /// `$display` text) — see [`MessageSeverity`]. `scope` is the instance
    /// path the message was emitted from (dotted instance labels,
    /// top-relative, matching signal names) — `None` when unavailable:
    /// kernel guards, concurrent assigns, replayed `.hwf` transcripts.
    Message {
        text: String,
        time_fs: u64,
        span: Option<Span>,
        severity: Option<MessageSeverity>,
        scope: Option<String>,
    },
    /// A test failed: `assert`/`report` with severity `error`/`failure`, or a
    /// Verilog `$error`/`$fatal`. CLI runners exit non-zero when this fires.
    /// `span` is the failing statement's source span when known.
    TestFailed {
        text: String,
        time_fs: u64,
        span: Option<Span>,
    },
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
