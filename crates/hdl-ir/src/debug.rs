//! Lowerer debug traces, armed explicitly instead of via environment
//! variables: environment variables have no spell check, so a typo'd
//! `--trace-overload` silently traced nothing. The CLI sets these
//! from `--trace-*` options before compiling; library users can set them
//! the same way. Each flag is read on the (rare) diagnostic path only —
//! an `AtomicBool` load is the entire off-state cost.

use crate::value::{is_known, Value, V1};
use hdl_syntax::source::{SourceDb, Span};
use std::sync::atomic::{AtomicBool, Ordering};

/// `--trace-ce`: log user-function calls evaluated by the constant
/// evaluator (functions named `get_*` always; all functions with
/// [`TRACE_CE_ALL`]).
pub static TRACE_CE: AtomicBool = AtomicBool::new(false);

/// `--trace-ce-all`: with [`TRACE_CE`], log every const-evaluated
/// function call, not just `get_*`.
pub static TRACE_CE_ALL: AtomicBool = AtomicBool::new(false);

/// `--trace-overload`: log overload resolution when several same-name
/// function candidates exist (picked vs skipped bodies).
pub static TRACE_OVERLOAD: AtomicBool = AtomicBool::new(false);

/// `--trace-genrange`: log large generate-loop ranges (n > 64) with
/// their bounds and source location.
pub static TRACE_GENRANGE: AtomicBool = AtomicBool::new(false);

/// `--trace-lower`: log static-attribute dimension resolution
/// (`'length(d)`-family) — which dims source answered (signal constraint /
/// object dims / type dims / type info) — and the population of `sig_dims`
/// from an object's own inline constraint. The lower-side half of the
/// diagnosis trail; `hdlsim ir --dump-maps` is the static half.
pub static TRACE_LOWER: AtomicBool = AtomicBool::new(false);

/// `--dump-consts`: print every constant/generic value at the moment the
/// lowerer binds it (scope, kind, source line, width, fold status, value).
/// This is the elaboration-time view behind W0401 cascades: the first X
/// constant printed above a failure is the root.
pub static DUMP_CONSTS: AtomicBool = AtomicBool::new(false);

/// With [`DUMP_CONSTS`], emit the records as JSON Lines (the
/// `--error-format json` counterpart) instead of text, so agents can parse
/// them without scraping.
pub static DUMP_CONSTS_JSON: AtomicBool = AtomicBool::new(false);

/// Set any subset of the trace flags in one call (`true` = on).
pub fn set_traces(ce: bool, ce_all: bool, overload: bool, genrange: bool, lower: bool) {
    TRACE_CE.store(ce, Ordering::Relaxed);
    TRACE_CE_ALL.store(ce_all, Ordering::Relaxed);
    TRACE_OVERLOAD.store(overload, Ordering::Relaxed);
    TRACE_GENRANGE.store(genrange, Ordering::Relaxed);
    TRACE_LOWER.store(lower, Ordering::Relaxed);
}

/// Arm [`DUMP_CONSTS`] and its JSON variant — one call from
/// `apply_global_args` after both `--dump-consts` and `--error-format` are
/// known, whichever order the flags appear in.
pub fn set_dump_consts(on: bool, json: bool) {
    DUMP_CONSTS.store(on, Ordering::Relaxed);
    DUMP_CONSTS_JSON.store(on && json, Ordering::Relaxed);
}

/// One `--dump-consts` record: a constant, generic, or function-local
/// constant at the moment the lowerer binds its final value.
pub struct ConstDump<'a> {
    /// Package name, or the `entity` / `entity/arch` module being lowered.
    pub scope: &'a str,
    pub name: &'a str,
    /// `constant` | `generic` | `local` (function-local constant).
    pub kind: &'a str,
    pub span: Span,
    pub width: usize,
    /// `ok` (all bits known) | `x` (meta bits in the value) | `unfolded`
    /// (initializer was not const-evalable; bound to X) | `unset` (generic
    /// with no known value) | `dynamic` (function-local constant with a
    /// runtime slot).
    pub status: &'a str,
    pub value: &'a Value,
}

/// Values past this many bits summarize instead of printing full bit
/// patterns (a 32-kibibit record constant must not print a 16-kilobyte
/// line, and an all-zero giant's hex suppresses to `0x0`).
const MAX_VALUE_TEXT_BITS: usize = 256;

/// Hex head/tail characters kept when a known value's hex text still
/// exceeds the budget above: leading-zero suppression makes the printed
/// length independent of the width, so the guard goes on the text length.
const HALF_VALUE_TEXT_CHARS: usize = 48;

/// Print one record when `--dump-consts` is armed; an atomic load otherwise.
pub fn dump_const(db: &SourceDb, d: ConstDump) {
    if !DUMP_CONSTS.load(Ordering::Relaxed) {
        return;
    }
    let f = db.file(d.span.file);
    let (line, _) = f.line_col(d.span.start);
    let val = fmt_const_value(d.value);
    if DUMP_CONSTS_JSON.load(Ordering::Relaxed) {
        let mut obj = serde_json::json!({
            "cd": {
                "scope": d.scope,
                "name": d.name,
                "kind": d.kind,
                "file": f.name(),
                "line": line,
                "width": d.width,
                "status": d.status,
                "value": val,
            }
        });
        if let Some(a) = ascii_of(d.value) {
            obj["cd"]["ascii"] = serde_json::Value::String(a);
        }
        eprintln!("{obj}");
        return;
    }
    let ascii = ascii_of(d.value)
        .map(|a| format!(" ascii=\"{a}\""))
        .unwrap_or_default();
    eprintln!(
        "cd {}.{} kind={} width={} status={} value={val}{ascii} at {}:{line}",
        d.scope,
        d.name,
        d.kind,
        d.width,
        d.status,
        f.name(),
    );
}

/// [`Value::fmt_value`], summarized past [`MAX_VALUE_TEXT_BITS`] so a
/// record-sized constant stays one greppable line.
fn fmt_const_value(v: &Value) -> String {
    if v.width() <= MAX_VALUE_TEXT_BITS {
        return v.fmt_value();
    }
    if !v.is_known() {
        let unknown = v.bits.iter().filter(|b| !is_known(**b)).count();
        return format!("x{unknown}/{} bits", v.width());
    }
    let hex = v.fmt_value();
    if hex.len() <= 2 * HALF_VALUE_TEXT_CHARS {
        return hex;
    }
    let head = &hex[..HALF_VALUE_TEXT_CHARS];
    let tail = &hex[hex.len() - HALF_VALUE_TEXT_CHARS..];
    format!("{head}…{tail}({}-bit)", v.width())
}

/// The value's bits as printable ASCII when they decode as a character
/// string: byte-sized, all bytes printable, MSB-first like string literals
/// (`"Idle"` prints as ascii="Idle"). `None` otherwise.
fn ascii_of(v: &Value) -> Option<String> {
    let bits = &v.bits;
    if v.real || bits.is_empty() || bits.len() % 8 != 0 || !v.is_known() {
        return None;
    }
    let n = bits.len() / 8;
    let mut s = String::with_capacity(n);
    // String literals pack the first char in the high byte, and within a
    // byte bit `j` sits at `base + j` (LSB-first like the rest of `Value`).
    for bi in 0..n {
        let base = (n - 1 - bi) * 8;
        let mut b = 0u8;
        for j in 0..8 {
            b |= u8::from(bits[base + j] == V1) << j;
        }
        if !(0x20..0x7f).contains(&b) {
            return None;
        }
        s.push(b as char);
    }
    Some(s)
}
