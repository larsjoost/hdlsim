//! The Questa-style observation probes: `--when` echoes a line when a
//! signal changes into a matching value inside an optional time window, and
//! `--examine` prints one settled value at a time point (or end of run).
//!
//! Child module of `sim` so it can read the kernel's private fields, the
//! same pattern as `sim/ckpt.rs`.

use super::Simulator;
use hdl_ir::elab::FlatSigId;
use hdl_ir::fmt_time;
use hdl_ir::value::{Value, V0, V1, VDASH, VH, VL, VU, VW, VX, VZ};
use hdl_ir::Expr;

// ---- --when / --examine -----------------------------------------------------------

/// One resolved `--when` rule: echo a line when `sig` changes, optionally
/// only into value `want` (exact bit-state match, metavarues included) and
/// inside the time window (`@T` = exactly T, `@T0-T1` = inclusive range).
pub(super) struct WhenRule {
    sig: FlatSigId,
    name: String,
    want: Option<Value>,
    t0: Option<u64>,
    t1: Option<u64>,
}

/// One resolved `--examine` rule: print `name=value` once — at the settled
/// value for time point `at`, or the end-of-run value when `at` is None.
/// `printed` dedupes across settle points.
pub(super) struct ExamineRule {
    sig: FlatSigId,
    name: String,
    at: Option<u64>,
    printed: bool,
}

impl Simulator {
    /// rustc-style error for an unmatched single-name observation spec:
    /// the flag, the name, and up to three near-miss suggestions.
    fn unmatched_error(&self, flag: &str, name: &str) -> String {
        let mut msg = format!("{flag}: no signal matched {name}");
        let s = self.watch_suggestions(name);
        if !s.is_empty() {
            msg.push('\n');
            msg.push_str(&s);
        }
        msg
    }

    /// Arm `--when` rules from raw specs `name[=value][@T|@T0-T1]`. Echo
    /// lines print from `when_change` while the run executes. Unmatched
    /// names, unparseable times, and values that fit no matched signal are
    /// hard errors before the run starts — a typo'd knob must not fail
    /// silently.
    pub fn set_when_rules(&mut self, specs: &[String]) -> Result<(), String> {
        self.when_rules.clear();
        for spec in specs {
            let WhenParts {
                name,
                value,
                t0,
                t1,
            } = parse_when_spec(spec)?;
            let hits = self.resolve_signal_pattern(&name);
            if hits.is_empty() {
                self.when_rules.clear();
                return Err(self.unmatched_error("--when", &name));
            }
            for (sig, name) in hits {
                let want = match &value {
                    Some(v) => match parse_match_value(v, self.flat.signals[sig as usize].width) {
                        Some(w) => Some(w),
                        None => {
                            let w = self.flat.signals[sig as usize].width;
                            self.when_rules.clear();
                            return Err(format!(
                                "--when: cannot parse value '{v}' for {name} (width {w})"
                            ));
                        }
                    },
                    None => None,
                };
                self.when_rules.push(WhenRule {
                    sig,
                    name,
                    want,
                    t0,
                    t1,
                });
            }
        }
        eprintln!("when: {} rule(s) armed", self.when_rules.len());
        Ok(())
    }

    /// Arm `--examine` rules from raw specs `name[@T]`.
    pub fn set_examine_rules(&mut self, specs: &[String]) -> Result<(), String> {
        self.examine_rules.clear();
        for spec in specs {
            let (name, at) = match spec.rsplit_once('@') {
                Some((n, t)) => (
                    n.trim().to_string(),
                    Some(
                        hdl_ir::parse_time(t.trim())
                            .map_err(|e| format!("--examine: bad time in '{spec}': {e}"))?,
                    ),
                ),
                None => (spec.trim().to_string(), None),
            };
            let hits = self.resolve_signal_pattern(&name);
            if hits.is_empty() {
                self.examine_rules.clear();
                return Err(self.unmatched_error("--examine", &name));
            }
            for (sig, name) in hits {
                self.examine_rules.push(ExamineRule {
                    sig,
                    name,
                    at,
                    printed: false,
                });
            }
        }
        eprintln!("examine: {} rule(s) armed", self.examine_rules.len());
        Ok(())
    }

    /// One `when:` line per matching rule for a changed signal. Called from
    /// `set_sig` next to `watch_change`; the caller checks for empty rules.
    pub(super) fn when_change(&mut self, sig: FlatSigId, old: &Value, new: &Value) {
        for r in &self.when_rules {
            if r.sig != sig || !in_window(r.t0, r.t1, self.time) {
                continue;
            }
            if let Some(w) = &r.want {
                if new != w {
                    continue;
                }
            }
            eprintln!(
                "when t={} d{} {} {}->{}{}",
                fmt_time(self.time),
                self.delta_no,
                r.name,
                old.fmt_value(),
                new.fmt_value(),
                self.driver_suffix()
            );
        }
    }

    /// Print `--examine` rules due at this settle point: a rule for time
    /// `at` prints at the first settle whose next event lies beyond `at`,
    /// so the printed value is the settled value at exactly `at` — no
    /// later event has applied yet. Called from the pump next to
    /// `watch_snapshot`.
    pub(super) fn examine_settle(&mut self, next: Option<u64>) {
        if self.examine_rules.is_empty() {
            return;
        }
        let limit = next.unwrap_or(u64::MAX);
        for r in &mut self.examine_rules {
            if r.printed {
                continue;
            }
            let Some(at) = r.at else { continue };
            if at >= limit {
                continue;
            }
            r.printed = true;
            eprintln!(
                "examine t={} {}={}",
                fmt_time(at),
                r.name,
                self.vals[r.sig as usize].fmt_value()
            );
        }
    }

    /// End-of-run flush for `--examine` rules the settle points never
    /// printed: the run stopped before the requested time, or no settle
    /// point ran (no events, or an interrupt mid-timestamp). Every line
    /// shows the end-of-run value and says so.
    pub fn examine_flush_final(&mut self) {
        if self.examine_rules.iter().all(|r| r.printed) {
            return;
        }
        let end = self.time;
        for r in &self.examine_rules {
            if r.printed {
                continue;
            }
            let v = self.vals[r.sig as usize].fmt_value();
            match r.at {
                Some(at) if at > end => eprintln!(
                    "examine t={} {}={} (end-of-run value; t={} not reached)",
                    fmt_time(at),
                    r.name,
                    v,
                    fmt_time(at)
                ),
                // At or before the end: the settle point should have printed
                // it — this is the no-settle fallback (no events, interrupt).
                other => eprintln!(
                    "examine t={} {}={} (end-of-run value)",
                    fmt_time(other.unwrap_or(end)),
                    r.name,
                    v
                ),
            }
        }
    }
}

/// Parsed parts of one `--when` spec `name[=value][@T|@T0-T1]`.
struct WhenParts {
    name: String,
    value: Option<String>,
    t0: Option<u64>,
    t1: Option<u64>,
}

/// Parse one `--when` spec. `@T` is the exact-time window (`@10ns`),
/// `@T0-T1` the inclusive range; both parse through `hdl_ir::parse_time`.
fn parse_when_spec(spec: &str) -> Result<WhenParts, String> {
    let (head, t0, t1) = match spec.split_once('@') {
        Some((h, win)) => match win.split_once('-') {
            Some((a, b)) => (
                h,
                Some(parse_when_time(a, spec)?),
                Some(parse_when_time(b, spec)?),
            ),
            None => (h, Some(parse_when_time(win, spec)?), None),
        },
        None => (spec, None, None),
    };
    let (name, value) = match head.split_once('=') {
        Some((n, v)) => (n.trim().to_string(), Some(v.trim().to_string())),
        None => (head.trim().to_string(), None),
    };
    Ok(WhenParts {
        name,
        value,
        t0,
        t1,
    })
}

fn parse_when_time(s: &str, spec: &str) -> Result<u64, String> {
    hdl_ir::parse_time(s.trim()).map_err(|e| format!("--when: bad time in '{spec}': {e}"))
}

/// `@T` (t1 = None) matches exactly T; `@T0-T1` the inclusive range; no
/// window matches every timestamp.
fn in_window(t0: Option<u64>, t1: Option<u64>, now: u64) -> bool {
    match (t0, t1) {
        (None, _) => true,
        (Some(a), None) => now == a,
        (Some(a), Some(b)) => now >= a && now <= b,
    }
}

/// Parse a `--when` value literal against a signal width: `0x…`/`0b…`
/// hex/binary, a decimal integer (two's-complement at the signal width,
/// negatives included), or — for width-1 signals — a single std_logic
/// state letter. Values that fit no form, or do not fit the width, are
/// None (an arm-time error for the caller).
fn parse_match_value(s: &str, width: usize) -> Option<Value> {
    let s = s.trim();
    if width == 1 {
        if let Some(st) = match_state_letter(s) {
            return Some(Value::from_bit(st));
        }
    }
    if let Some(rest) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        let u = u64::from_str_radix(rest, 16).ok()?;
        return fits_width(u, width).then(|| Value::from_u64(width, u));
    }
    if let Some(rest) = s.strip_prefix("0b").or_else(|| s.strip_prefix("0B")) {
        let u = u64::from_str_radix(rest, 2).ok()?;
        return fits_width(u, width).then(|| Value::from_u64(width, u));
    }
    s.parse::<i64>().ok().and_then(|v| {
        if v >= 0 {
            fits_width(v as u64, width).then(|| Value::from_u64(width, v as u64))
        } else {
            // Two's complement at the signal width: -1 fits any width ≥ 1.
            let min = if width >= 64 {
                i64::MIN
            } else {
                -(1i64 << (width - 1))
            };
            (v >= min).then(|| Value::from_u64(width, v as u64))
        }
    })
}

/// Whether `u` is representable in `width` bits (64-bit values always are
/// from the caller's perspective — `from_u64` keeps the low bits).
fn fits_width(u: u64, width: usize) -> bool {
    width >= 64 || u < (1u64 << width)
}

/// Std_logic state letter (case-insensitive) to its bit-state constant.
fn match_state_letter(s: &str) -> Option<u8> {
    match s.to_ascii_uppercase().as_str() {
        "0" => Some(V0),
        "1" => Some(V1),
        "X" => Some(VX),
        "Z" => Some(VZ),
        "W" => Some(VW),
        "L" => Some(VL),
        "H" => Some(VH),
        "-" => Some(VDASH),
        "U" => Some(VU),
        _ => None,
    }
}

// ---- --trace-assign ---------------------------------------------------------------

/// One resolved `--trace-assign` rule: log every evaluation of the
/// continuous assignments driving `target`, with the values the RHS read.
pub(super) struct TraceAssign {
    target: FlatSigId,
    name: String,
}

/// The simple signal an assignment drives: `Expr::Sig` for plain targets;
/// None for bit-selects and the like, which the per-evaluation trace cannot
/// attribute.
fn assign_target_sig(lhs: &Expr) -> Option<FlatSigId> {
    match lhs {
        Expr::Sig(s) => Some(*s),
        _ => None,
    }
}

impl Simulator {
    /// Arm `--trace-assign` rules from raw signal-name specs (dot-suffix
    /// match like --watch). Unmatched names, and names no continuous
    /// assignment drives, are hard errors before the run starts — a typo'd
    /// knob must not fail silently.
    pub fn set_trace_assigns(&mut self, specs: &[String]) -> Result<(), String> {
        self.trace_assigns.clear();
        if specs.is_empty() {
            return Ok(());
        }
        let targets: Vec<Option<FlatSigId>> = self
            .flat
            .assigns
            .iter()
            .map(|a| assign_target_sig(&a.lhs))
            .collect();
        for spec in specs {
            let hits = self.resolve_signal_pattern(spec.trim());
            if hits.is_empty() {
                self.trace_assigns.clear();
                return Err(self.unmatched_error("--trace-assign", spec.trim()));
            }
            for (sig, name) in hits {
                if !targets.contains(&Some(sig)) {
                    self.trace_assigns.clear();
                    return Err(format!(
                        "--trace-assign: no continuous assignment drives {name} \
                         (process-driven signals: use --watch or --when)"
                    ));
                }
                self.trace_assigns.push(TraceAssign { target: sig, name });
            }
        }
        eprintln!("trace-assign: {} rule(s) armed", self.trace_assigns.len());
        Ok(())
    }

    /// One `trace-assign:` line per evaluation of a traced continuous
    /// assignment — including re-evaluations whose result equals the
    /// current value, because the silence around a sensitivity bug is
    /// itself the evidence. Called from the fixpoint sweep after the RHS
    /// is evaluated and converted, before the result is applied, so the
    /// `reads{}` values are exactly what the evaluation saw.
    pub(super) fn assign_trace_eval(&self, ai: usize, lhs: &Expr, rhs: &Expr, result: &Value) {
        let Some(sig) = assign_target_sig(lhs) else {
            return;
        };
        let Some(rule) = self.trace_assigns.iter().find(|r| r.target == sig) else {
            return;
        };
        let mut sigs = Vec::new();
        rhs.collect_sigs(&mut sigs);
        sigs.sort_unstable();
        sigs.dedup();
        let reads: Vec<String> = sigs
            .iter()
            .map(|s| {
                format!(
                    "{}={}",
                    self.flat.signals[*s as usize].name,
                    self.vals[*s as usize].fmt_value()
                )
            })
            .collect();
        eprintln!(
            "trace-assign t={} d{} sweep{} assign#{ai} ->{} = {} (was {}) reads{{{}}}",
            fmt_time(self.time),
            self.delta_no,
            self.fix_sweep,
            rule.name,
            result.fmt_value(),
            self.vals[sig as usize].fmt_value(),
            reads.join(", ")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{in_window, parse_match_value, parse_when_spec};
    use hdl_ir::value::VZ;

    #[test]
    fn when_spec_parses_name_value_and_windows() {
        let p = parse_when_spec("q=5@10ns-20ns").expect("parses");
        assert_eq!(p.name, "q");
        assert_eq!(p.value.as_deref(), Some("5"));
        assert_eq!(p.t0, Some(10_000_000));
        assert_eq!(p.t1, Some(20_000_000));

        let p = parse_when_spec("q@15ns").expect("parses");
        assert_eq!(p.t0, Some(15_000_000));
        assert_eq!(p.t1, None);

        let p = parse_when_spec("tb.dut.q").expect("parses");
        assert_eq!(p.name, "tb.dut.q");
        assert_eq!(p.value, None);
        assert_eq!(p.t0, None);
    }

    #[test]
    fn when_spec_rejects_bad_times() {
        assert!(parse_when_spec("q@abc").is_err());
        assert!(parse_when_spec("q@10ns-").is_err());
        assert!(parse_when_spec("q@").is_err());
    }

    #[test]
    fn window_matches_exact_range_and_always() {
        assert!(in_window(None, None, 0));
        assert!(in_window(Some(10), None, 10));
        assert!(!in_window(Some(10), None, 11));
        assert!(in_window(Some(10), Some(20), 10));
        assert!(in_window(Some(10), Some(20), 20));
        assert!(!in_window(Some(10), Some(20), 21));
    }

    #[test]
    fn match_values_parse_hex_bin_decimal_and_state_letters() {
        // Width 4: hex, binary, and decimal land on the same bits.
        for lit in ["0xA", "0b1010", "10"] {
            let v = parse_match_value(lit, 4).unwrap_or_else(|| panic!("{lit} parses"));
            assert_eq!(v.to_u64(), Some(10), "{lit}");
        }
        // Negative decimal two's-complements at the signal width.
        let v = parse_match_value("-1", 32).expect("-1 parses");
        assert_eq!(v.to_u64(), Some(u32::MAX as u64));
        // Width-1 state letters, case-insensitive; X has no numeric value.
        assert_eq!(parse_match_value("X", 1).expect("parses").to_u64(), None);
        assert_eq!(parse_match_value("z", 1).expect("parses").bit(0), VZ);
        // Values that fit no form, or do not fit the width, are rejected.
        assert_eq!(parse_match_value("zz", 4), None);
        assert_eq!(parse_match_value("0x10", 4), None);
        assert_eq!(parse_match_value("-16", 4), None);
    }
}
