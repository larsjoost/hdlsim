//! VCD (Value Change Dump) sink — IEEE 1364 waveform interchange format.
//!
//! Written incrementally as events arrive: `$scope`/`$var` header from the
//! elaborated hierarchy, `#<time>` on time advance, value changes on signal
//! events. One-bit signals use the `0!` short form, wider ones `b<bits> !`,
//! real signals (Verilog `real` / VHDL `real`) use `$var real` + `r<value>`.

use super::WaveSink;
use hdl_ir::elab::Flat;
use hdl_ir::value::{Value, V1, VZ};
use hdl_ir::SigKind;
use hdl_sim::events::SimEvent;
use std::io::Write;

pub struct VcdSink<W: Write> {
    w: W,
    /// VCD id code per flat signal.
    ids: Vec<String>,
    started: bool,
    /// Current VCD timestamp line written.
    last_time: Option<u64>,
}

impl<W: Write> VcdSink<W> {
    pub fn new(w: W, flat: &Flat) -> VcdSink<W> {
        let mut ids = Vec::with_capacity(flat.signals.len());
        let mut next = 0usize;
        for _ in 0..flat.signals.len() {
            // VCD id codes: printable ASCII 33..126, then multi-char.
            let mut code = String::new();
            let mut n = next;
            loop {
                code.push(std::char::from_u32(33 + (n % 94) as u32).unwrap_or('!'));
                n /= 94;
                if n == 0 {
                    break;
                }
            }
            ids.push(code);
            next += 1;
        }
        VcdSink {
            w,
            ids,
            started: false,
            last_time: None,
        }
    }

    fn write_header(&mut self, flat: &Flat) {
        let _ = writeln!(self.w, "$date");
        let _ = writeln!(self.w, "    hdlsim");
        let _ = writeln!(self.w, "$end");
        let _ = writeln!(self.w, "$version");
        let _ = writeln!(self.w, "    hdlsim {}", env!("CARGO_PKG_VERSION"));
        let _ = writeln!(self.w, "$end");
        let _ = writeln!(self.w, "$timescale 1fs $end");
        // Scope tree.
        let mut scope_stack: Vec<u32> = Vec::new();
        for (id, scope) in flat.scopes.iter().enumerate() {
            while scope_stack.len() > 1
                && scope.parent != scope_stack.get(scope_stack.len() - 1).copied()
            {
                let _ = writeln!(self.w, "$upscope $end");
                scope_stack.pop();
            }
            if id as u32 == flat.top_scope {
                let _ = writeln!(self.w, "$scope module {} $end", scope.name);
                scope_stack.push(id as u32);
            } else if let Some(p) = scope.parent {
                let _ = writeln!(self.w, "$scope module {} $end", scope.name);
                let _ = p;
                scope_stack.push(id as u32);
            }
        }
        for _ in 1..scope_stack.len() {
            let _ = writeln!(self.w, "$upscope $end");
        }
        // Vars: one per signal, named by its last path component; scope is flat
        // here for phase 1 (full nesting in the HTML viewer).
        for (i, sig) in flat.signals.iter().enumerate() {
            let name = sig.name.rsplit('.').next().unwrap_or(&sig.name);
            let w = sig.width.max(1);
            if sig.kind == SigKind::Real {
                let _ = writeln!(self.w, "$var real {} {} {} $end", w, self.ids[i], name);
            } else {
                let _ = writeln!(self.w, "$var wire {} {} {} $end", w, self.ids[i], name);
            }
        }
        let _ = writeln!(self.w, "$enddefinitions $end");
        let _ = writeln!(self.w, "$dumpvars");
        // Initial values at time 0.
        for (i, sig) in flat.signals.iter().enumerate() {
            let _ = write!(
                self.w,
                "{}",
                fmt_value(
                    &sig.init
                        .clone()
                        .unwrap_or_else(|| Value::x(sig.width.max(1))),
                    &self.ids[i],
                    sig.kind == SigKind::Real
                )
            );
        }
        let _ = writeln!(self.w);
        self.started = true;
    }

    fn ensure_header(&mut self, flat: &Flat) {
        if !self.started {
            self.write_header(flat);
        }
    }

    fn time_line(&mut self, t: u64) {
        if self.last_time != Some(t) {
            let _ = writeln!(self.w, "#{}", t);
            self.last_time = Some(t);
        }
    }
}

impl<W: Write> WaveSink for VcdSink<W> {
    fn on_event(&mut self, ev: &SimEvent, flat: &Flat) {
        match ev {
            SimEvent::TimeAdvance { time_fs } => {
                self.ensure_header(flat);
                self.time_line(*time_fs);
            }
            SimEvent::SignalChange {
                sig, new, time_fs, ..
            } => {
                self.ensure_header(flat);
                self.time_line(*time_fs);
                let is_real = flat.signals[*sig as usize].kind == SigKind::Real;
                let _ = write!(
                    self.w,
                    "{}",
                    fmt_value(new, &self.ids[*sig as usize], is_real)
                );
            }
            SimEvent::Finish { time_fs } | SimEvent::Stop { time_fs } => {
                self.time_line(*time_fs);
            }
            _ => {}
        }
    }
}

fn fmt_value(v: &Value, id: &str, is_real: bool) -> String {
    if is_real {
        return match v.to_f64() {
            Some(f) => format!("r{} {}\n", fmt_f64(f), id),
            None => format!("rx {}\n", id),
        };
    }
    if v.width() == 1 {
        let c = match v.bit(0) {
            V1 => '1',
            VZ => 'z',
            b if b == hdl_ir::value::V0 => '0',
            _ => 'x',
        };
        format!("{}{}\n", c, id)
    } else {
        let s: String = v
            .bits
            .iter()
            .rev()
            .map(|&b| match b {
                V1 => '1',
                hdl_ir::value::V0 => '0',
                VZ => 'z',
                _ => 'x',
            })
            .collect();
        format!("b{} {}\n", s, id)
    }
}

/// Format an f64 for VCD `r` values: shortest round-trip decimal; non-finite
/// values fall back to a large placeholder (VCD has no inf/nan syntax).
fn fmt_f64(f: f64) -> String {
    if f.is_finite() {
        let mut s = format!("{}", f);
        if !s.contains('.') && !s.contains('e') && !s.contains('E') {
            s.push_str(".0");
        }
        s
    } else if f.is_nan() {
        "NaN".to_string()
    } else if f > 0.0 {
        "1e308".to_string()
    } else {
        "-1e308".to_string()
    }
}
