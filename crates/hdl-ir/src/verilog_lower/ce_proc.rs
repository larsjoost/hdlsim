//! Const-eval of user procedure call statements — the statement twin of the
//! `Expr::Call` function dispatch. A procedure runs on a fresh frame built
//! from the actuals; `inout`/`out` formals copy their final values back into
//! the caller's slots (LRM 1076 §4.3), the const-eval mirror of `hdl-sim`'s
//! `proc_copyback`. Child module of `verilog_lower` so it can reuse the
//! private const-eval engine (`ce_impl`, `ce_stmt`, `CeFlow`, depth guard).

use std::collections::HashMap;
use std::rc::Rc;

use crate::ir::{ArgMode, Expr, FuncIr, SigId, Stmt};
use crate::value::{Value, VX};

use super::{ce_impl, ce_stmt, CeFlow, CE_DEPTH, CE_MAX_DEPTH};

/// Run a user procedure call statement at const-eval: pick a same-arity
/// real body, run it on a fresh frame, and copy `inout`/`out` formals back
/// to the caller's slots. `false` = no resolvable procedure (a builtin
/// syscall, an unknown name, or a failing body) — the caller aborts the fold.
pub(super) fn ce_call_procedure(
    name: &str,
    args: &[Expr],
    slots: &mut Vec<Value>,
    env: &HashMap<String, Value>,
    funcs: &HashMap<String, Vec<FuncIr>>,
    extras: &HashMap<String, Vec<Rc<FuncIr>>>,
) -> bool {
    let Some(f) = pick_procedure(name, args.len(), funcs, extras) else {
        return false;
    };
    // Same shared budget as the function dispatch: a self-recursive or
    // mutually recursive procedure aborts the fold instead of overflowing.
    let depth = CE_DEPTH.with(|d| d.get());
    if depth >= CE_MAX_DEPTH {
        return false;
    }
    // Callee frame: slot 0 is the unused return slot, then one slot per
    // actual (an actual wider than the declared formal keeps its width —
    // unconstrained string parameters), then the body's locals.
    let mut lvals: Vec<Value> = vec![Value::x(f.ret_width.max(1))];
    for (i, a) in args.iter().enumerate() {
        let w = f.args.get(i).map(|(_, w, _)| *w).unwrap_or(1);
        match ce_impl(a, env, funcs, extras, Some(&mut *slots)) {
            Some(v) => lvals.push(v.resize(w.max(v.width()))),
            None => return false,
        }
    }
    // Trailing formals omitted at the call site take their interface
    // defaults (`step : in integer := 2`) — `pick_procedure` checked that
    // every omitted formal carries one.
    for d in f.defaults.iter().skip(args.len()) {
        match d {
            Some(d) => lvals.push(d.clone()),
            None => break,
        }
    }
    lvals.extend(
        f.locals
            .iter()
            .map(|l| l.init.clone().unwrap_or_else(|| Value::x(l.width.max(1)))),
    );
    if !fold_dyn_locals(&f.dyn_locals, &mut lvals, env, funcs, extras) {
        return false;
    }
    CE_DEPTH.with(|d| d.set(depth + 1));
    let mut sub = CeFlow::Continue;
    ce_stmt(&f.body, &mut lvals, env, funcs, extras, &mut sub);
    CE_DEPTH.with(|d| d.set(depth));
    // `return;` is a normal procedure completion (LRM 1076 §10.2) — the
    // copy-back below still runs; an abort or an escaped break/continue
    // fails the fold.
    if !matches!(sub, CeFlow::Continue | CeFlow::Return) {
        return false;
    }
    for (i, a) in args.iter().enumerate() {
        if matches!(f.modes.get(i), Some(ArgMode::Inout | ArgMode::Out)) {
            let Some(v) = lvals.get(i + 1).cloned() else {
                continue;
            };
            if !copy_back(a, v, slots, env, funcs, extras) {
                return false;
            }
        }
    }
    true
}

/// Re-fold a local's formal-dependent constraint (`string(1 to size)`)
/// against the frame: registration folded it to the fallback width because
/// the formals had no values yet. `false` = unfoldedable or absurdly wide —
/// the fold aborts. Shared with `const_eval_func`.
pub(super) fn fold_dyn_locals(
    dyn_locals: &[(SigId, Expr)],
    slots: &mut Vec<Value>,
    env: &HashMap<String, Value>,
    funcs: &HashMap<String, Vec<FuncIr>>,
    extras: &HashMap<String, Vec<Rc<FuncIr>>>,
) -> bool {
    for (slot, wexpr) in dyn_locals {
        let w = ce_impl(wexpr, env, funcs, extras, Some(&mut *slots))
            .as_ref()
            .and_then(|v| v.to_u64());
        let Some(w) = w else { continue };
        if w > (1 << 24) {
            return false;
        }
        let s = *slot as usize;
        while slots.len() <= s {
            slots.push(Value::x(32));
        }
        slots[s] = Value::x(w.max(1) as usize);
    }
    true
}

/// Overload pick for a procedure statement: same arity (trailing formals may
/// be covered by interface defaults), a real body over an interface-only
/// stub — matching the `Expr::Call` dispatch's ordering; directly visible
/// candidates first, the design-wide package fallback on a total miss.
fn pick_procedure<'f>(
    name: &str,
    arity: usize,
    funcs: &'f HashMap<String, Vec<FuncIr>>,
    extras: &'f HashMap<String, Vec<Rc<FuncIr>>>,
) -> Option<&'f FuncIr> {
    let is_stub = |f: &FuncIr| match &f.body {
        Stmt::Null => true,
        Stmt::Block(ss) => ss.is_empty(),
        _ => false,
    };
    let fits = |f: &FuncIr| {
        f.args.len() >= arity
            && f.defaults
                .get(arity..)
                .is_some_and(|t| t.iter().all(|d| d.is_some()))
            && !is_stub(f)
    };
    funcs
        .get(&name.to_ascii_lowercase())
        .into_iter()
        .flatten()
        .find(|f| fits(f))
        .or_else(|| {
            extras
                .get(&name.to_ascii_lowercase())
                .into_iter()
                .flatten()
                .map(Rc::as_ref)
                .find(|f| fits(f))
        })
}

/// Copy a formal's final value back to the caller's actual: a whole slot
/// replaces, an element (`v(i)`) or slice (`v(hi downto lo)`) splices at the
/// bounds as they evaluate now — the shapes the caller's Assign arm writes.
/// Other actuals are expressions with no variable to receive the copy.
/// `false` = a misfolded index — the caller aborts the fold.
fn copy_back(
    actual: &Expr,
    v: Value,
    slots: &mut Vec<Value>,
    env: &HashMap<String, Value>,
    funcs: &HashMap<String, Vec<FuncIr>>,
    extras: &HashMap<String, Vec<Rc<FuncIr>>>,
) -> bool {
    let ev = |e: &Expr, slots: &mut Vec<Value>| ce_impl(e, env, funcs, extras, Some(slots));
    match actual {
        Expr::Sig(s) => {
            let s = *s as usize;
            while slots.len() <= s {
                slots.push(Value::x(32));
            }
            slots[s] = v.resize(slots[s].width().max(1));
            true
        }
        Expr::Idx { e, i } => {
            if let (Expr::Sig(s), Some(idx)) = (&**e, ev(i, slots).and_then(|x| x.to_u64())) {
                splice_at(slots, *s as usize, idx as usize, &v)
            } else {
                true
            }
        }
        Expr::Sel { e, msb, lsb } => {
            if let Expr::Sig(s) = &**e {
                if let (Some(m), Some(l)) = (
                    ev(msb, slots).and_then(|x| x.to_u64()),
                    ev(lsb, slots).and_then(|x| x.to_u64()),
                ) {
                    let w = (m.saturating_sub(l) + 1) as usize;
                    return splice_at(slots, *s as usize, l as usize, &v.resize(w));
                }
            }
            true
        }
        _ => true,
    }
}

/// Splice `v` into slot `s` at bit offset `off`, growing the slot with X
/// when the write runs past its end. An offset beyond a few MB is a
/// misfolded negative index, never a real constant — `false` aborts the
/// fold instead of growing the slot by gigabytes.
fn splice_at(slots: &mut Vec<Value>, s: usize, off: usize, v: &Value) -> bool {
    let w = v.width().max(1);
    if off + w > (1 << 24) {
        return false;
    }
    while slots.len() <= s {
        slots.push(Value::x(32));
    }
    if slots[s].bits.len() < off + w {
        let mut bits = slots[s].bits.clone();
        bits.resize(off + w, VX);
        slots[s] = Value { bits, real: false };
    }
    for b in 0..w {
        slots[s].set_bit(off + b, v.bit(b));
    }
    true
}
