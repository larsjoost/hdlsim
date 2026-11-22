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

#[cfg(test)]
mod tests {
    use super::fmt_time;

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
}
