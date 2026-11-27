//! # hdl-ir
//!
//! Language-neutral elaborated design IR: modules, instances, signals, processes,
//! expressions, parameters. Both the Verilog and the VHDL front-end lower onto
//! this IR, which is what makes mixed-language simulation native.
//!
//! Pipeline position: after `hdl-lib` resolution, before `hdl-sim`.
//!
//! Extension seam: `trait Lowering` — a new front-end language implements it to
//! produce IR. The IR types themselves are a closed core (the compiler checks
//! every match), dumpable to text so other tools (synthesis, formal, coverage)
//! can consume a whole elaborated design without parsing HDL.
//!
//! # Layout
//! - [`value`]: multi-state logic values (9-state capable, Verilog uses 4).
//! - [`ir`]: the module-level IR (pre-elaboration).
//! - [`elab`]: elaboration — flattens instance hierarchy into one signal space.
//!
//! # Invariants
//! - Lowering is deterministic and uses only the passed `ResolveCtx` for names.
//! - All ids (`SigId`, `ProcId`, …) are arena indices, never pointers.
//! - After elaboration, every `Expr::Sig` refers to the flat signal space.

pub mod analyze;
pub mod debug;
pub mod doc;
pub mod elab;
pub mod gen;
pub mod ir;
pub mod value;
pub mod verilog_lower;
pub mod vhdl_lower;

pub use ir::*;
pub use value::{BinOp, UnOp, Value};

/// Format a femtosecond simulation time readably with an auto-scaled unit:
/// `59918000000000` → `"59.918ms"`, `1000000` → `"1ns"`. Used by
/// diagnostics, heartbeats, and phase summaries so `t=` values are
/// comparable with `--max-time` arguments instead of raw fs counts.
/// Levenshtein edit distance, counted in chars, but gives up (returns
/// `max + 1`) as soon as the distance provably exceeds `max` — keeps
/// "is it close?" cheap on long candidate lists (did-you-mean suggestions
/// in the lowerers and in the simulator's `--watch` error).
pub fn levenshtein_bounded(a: &str, b: &str, max: usize) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = Vec::with_capacity(b.len() + 1);
        cur.push(i + 1);
        for (j, cb) in b.iter().enumerate() {
            cur.push(
                (prev[j] + usize::from(ca != *cb))
                    .min(prev[j + 1] + 1)
                    .min(cur[j] + 1),
            );
        }
        if cur.iter().min().copied().unwrap_or(usize::MAX) > max {
            return max + 1;
        }
        prev = cur;
    }
    prev[b.len()]
}

pub fn fmt_time(fs: u64) -> String {
    const UNITS: [(u64, &str); 5] = [
        (1_000_000_000_000_000, "s"),
        (1_000_000_000_000, "ms"),
        (1_000_000_000, "us"),
        (1_000_000, "ns"),
        (1_000, "ps"),
    ];
    for (div, unit) in UNITS {
        if fs >= div {
            let mut v = format!("{:.3}", fs as f64 / div as f64);
            if v.contains('.') {
                while v.ends_with('0') {
                    v.pop();
                }
                if v.ends_with('.') {
                    v.pop();
                }
            }
            return format!("{v}{unit}");
        }
    }
    format!("{fs}fs")
}

/// Parse a time like `100ns`, `10us`, `1ms`, `500` (bare = ns) into
/// femtoseconds. The inverse of [`fmt_time`] for the values it prints
/// (`59.918ms` and the like round-trip).
pub fn parse_time(s: &str) -> Result<u64, String> {
    let (num, unit) = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .map(|i| (&s[..i], &s[i..]))
        .unwrap_or((s, ""));
    let v: f64 = num.parse().map_err(|_| format!("bad time value '{s}'"))?;
    let mult: f64 = match unit {
        "" | "ns" => 1e6,
        "fs" => 1.0,
        "ps" => 1e3,
        "us" | "µs" => 1e9,
        "ms" => 1e12,
        "s" => 1e15,
        _ => return Err(format!("unknown time unit '{unit}'")),
    };
    Ok((v * mult) as u64)
}

#[cfg(test)]
mod tests {
    use super::{fmt_time, parse_time};

    #[test]
    fn fmt_time_picks_the_largest_unit_that_fits() {
        assert_eq!(fmt_time(0), "0fs");
        assert_eq!(fmt_time(999), "999fs");
        assert_eq!(fmt_time(1_000), "1ps");
        assert_eq!(fmt_time(20_000_000), "20ns");
        assert_eq!(fmt_time(59_918_000_000_000), "59.918ms");
        assert_eq!(fmt_time(63_891_000_000_000), "63.891ms");
        assert_eq!(fmt_time(3_600_000_000_000_000), "3.6s");
    }

    #[test]
    fn parses_times() {
        // Times are femtoseconds: 1 fs = 1, 1 ps = 1e3, 1 ns = 1e6, …
        assert_eq!(parse_time("100ns").unwrap(), 100_000_000);
        assert_eq!(parse_time("10us").unwrap(), 10_000_000_000);
        assert_eq!(parse_time("500").unwrap(), 500_000_000);
        assert_eq!(parse_time("2ms").unwrap(), 2_000_000_000_000);
        assert_eq!(parse_time("3ps").unwrap(), 3_000);
        assert!(parse_time("10xs").is_err());
    }
}
