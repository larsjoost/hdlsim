//! Lowerer debug traces, armed explicitly instead of via environment
//! variables: environment variables have no spell check, so a typo'd
//! `--trace-overload` silently traced nothing. The CLI sets these
//! from `--trace-*` options before compiling; library users can set them
//! the same way. Each flag is read on the (rare) diagnostic path only —
//! an `AtomicBool` load is the entire off-state cost.

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

/// Set any subset of the trace flags in one call (`true` = on).
pub fn set_traces(ce: bool, ce_all: bool, overload: bool, genrange: bool, lower: bool) {
    TRACE_CE.store(ce, Ordering::Relaxed);
    TRACE_CE_ALL.store(ce_all, Ordering::Relaxed);
    TRACE_OVERLOAD.store(overload, Ordering::Relaxed);
    TRACE_GENRANGE.store(genrange, Ordering::Relaxed);
    TRACE_LOWER.store(lower, Ordering::Relaxed);
}
