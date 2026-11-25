//! Debugging helpers — usable both from the CLI (`hdlsim debug`) and from your
//! own Rust code embedding `hdl-sim`: format values/times, watch signal changes
//! through the observer bus, and inspect the elaborated design.
//!
//! ```
//! use hdl_sim::debug::{fmt_value, Watcher};
//! use hdl_ir::Value;
//! assert_eq!(fmt_value(&Value::from_u64(8, 0xab)), "0xAB");
//! ```

use crate::events::{Observer, SimEvent};
use hdl_ir::elab::Flat;
use hdl_ir::Value;

const BIT_CHARS: [char; 9] = ['0', '1', 'x', 'z', 'w', 'L', 'H', '-', 'U'];

/// Format a value like a wave viewer: `0xAB` when every bit is 0/1, otherwise a
/// per-bit string (`bits[0]` = LSB printed rightmost), e.g. `"10x1"`.
pub fn fmt_value(v: &Value) -> String {
    if v.bits.iter().all(|b| *b == 0 || *b == 1) {
        if v.bits.len() > 64 {
            // Wide clean value (e.g. a memory init): show the low 64 bits.
            let low = Value {
                bits: v.bits[..64].to_vec(),
                real: false,
            };
            return format!("{}…({} bits)", fmt_value(&low), v.bits.len());
        }
        let mut hex = String::with_capacity(v.bits.len() / 4 + 3);
        hex.push_str("0x");
        let nibbles = v.bits.len().div_ceil(4);
        for n in (0..nibbles).rev() {
            let mut val = 0u8;
            for k in 0..4 {
                let i = n * 4 + k;
                if i < v.bits.len() && v.bits[i] == 1 {
                    val |= 1 << k;
                }
            }
            hex.push(
                char::from_digit(val as u32, 16)
                    .expect("val is a 4-bit nibble, so from_digit(16) succeeds")
                    .to_ascii_uppercase(),
            );
        }
        hex
    } else {
        let mut s = String::with_capacity(v.bits.len());
        for b in v.bits.iter().rev() {
            s.push(BIT_CHARS[(*b as usize).min(8)]);
        }
        s
    }
}

/// Format a femtosecond timestamp in the largest readable unit.
pub fn fmt_time(fs: u64) -> String {
    if fs % 1_000_000_000_000_000 == 0 && fs >= 1_000_000_000_000_000 {
        format!("{}s", fs / 1_000_000_000_000_000)
    } else if fs % 1_000_000_000_000 == 0 && fs >= 1_000_000_000_000 {
        format!("{}ms", fs / 1_000_000_000_000)
    } else if fs % 1_000_000_000 == 0 && fs >= 1_000_000_000 {
        format!("{}us", fs / 1_000_000_000)
    } else if fs % 1_000_000 == 0 && fs >= 1_000_000 {
        format!("{}ns", fs / 1_000_000)
    } else {
        format!("{}fs", fs)
    }
}

/// An [`Observer`] that records (and optionally echoes) changes of watched
/// signals plus transcript messages — a text wave/log trace for debugging.
///
/// Watch patterns match either the full hierarchical name (`tb.dut.q`) or the
/// leaf name (`q`).
pub struct Watcher {
    names: Vec<String>,
    patterns: Vec<String>,
    watch_all: bool,
    echo: bool,
    lines: Vec<String>,
}

impl Watcher {
    pub fn new(flat: &Flat) -> Self {
        Watcher {
            names: flat.signals.iter().map(|s| s.name.clone()).collect(),
            patterns: Vec::new(),
            watch_all: false,
            echo: false,
            lines: Vec::new(),
        }
    }

    /// Watch one signal (full or leaf name). Repeatable.
    pub fn watch(mut self, pattern: &str) -> Self {
        self.patterns.push(pattern.to_string());
        self
    }

    /// Watch every signal change.
    pub fn watch_all(mut self) -> Self {
        self.watch_all = true;
        self
    }

    /// Also emit each line through the `log` facade as the event arrives
    /// (live tracing; the binary routes it to stderr).
    pub fn echo(mut self, on: bool) -> Self {
        self.echo = on;
        self
    }

    /// The recorded lines so far (also printed live when `echo(true)`).
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    fn emit(&mut self, line: String) {
        if self.echo {
            log::info!("{}", line);
        }
        self.lines.push(line);
    }

    fn watched(&self, sig: u32, name: &str) -> bool {
        if self.watch_all {
            return true;
        }
        self.patterns.iter().any(|p| {
            if let Some(num) = p.strip_prefix('#') {
                return num.parse::<u32>() == Ok(sig);
            }
            p == name || name.rsplit('.').next() == Some(p.as_str())
        })
    }
}

impl Observer for Watcher {
    fn on_event(&mut self, ev: &SimEvent) {
        match ev {
            SimEvent::SignalChange {
                sig, new, time_fs, ..
            } => {
                let name = self.names.get(*sig as usize).map_or("?", |s| s.as_str());
                if self.watched(*sig, name) {
                    self.emit(format!(
                        "t={:<10} {} = {}",
                        fmt_time(*time_fs),
                        name,
                        fmt_value(new)
                    ));
                }
            }
            SimEvent::Message { text, time_fs, .. } => {
                self.emit(format!(
                    "t={:<10} | {}",
                    fmt_time(*time_fs),
                    text.trim_end()
                ));
            }
            SimEvent::Finish { time_fs } => {
                self.emit(format!("t={:<10} $finish", fmt_time(*time_fs)));
            }
            SimEvent::Stop { time_fs } => {
                self.emit(format!(
                    "t={:<10} (stopped: no more events)",
                    fmt_time(*time_fs)
                ));
            }
            _ => {}
        }
    }
}
