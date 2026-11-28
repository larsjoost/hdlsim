//! `Expr::Call` dispatch for the const-eval engine: builtin system
//! functions, overload resolution across same-name candidates
//! (real bodies before interface stubs, width fit, scored fallback), the
//! recursive-call depth guard, and the `--trace-ce` log line. Split out of
//! `const_eval.rs` to keep both at the module-size standard.

use std::collections::HashMap;
use std::rc::Rc;

use crate::ir::{Expr, FuncIr, ShapeBind, Stmt};
use crate::value::Value;

use super::ce_func::const_eval_func;
use super::ce_proc::eval_hidden_binds;
use super::const_eval::{ce_impl, CE_DEPTH, CE_MAX_DEPTH};

pub(super) fn ce_call(
    name: &str,
    args: &[Expr],
    shapes: &[ShapeBind],
    env: &HashMap<String, Value>,
    funcs: &HashMap<String, Vec<FuncIr>>,
    extras: &HashMap<String, Vec<Rc<FuncIr>>>,
    mut slots: Option<&mut Vec<Value>>,
) -> Option<Value> {
    // System functions usable in constant expressions.
    // lower_expr strips the `$` from system tasks, so both spellings.
    if let Some(v) = ce_builtin(name, args, env, funcs, extras, slots.as_deref_mut()) {
        return v;
    }
    let cands = ce_candidates(name, funcs, extras)?;
    let mut argv = Vec::with_capacity(args.len());
    for a in args {
        argv.push(ce_impl(a, env, funcs, extras, slots.as_deref_mut())?);
    }
    let f = ce_pick(&cands, &argv)?;
    // Fill the omitted actuals from the recorded defaults.
    ce_fill_defaults(f, &mut argv);
    let depth = CE_DEPTH.with(|d| d.get());
    if depth >= CE_MAX_DEPTH {
        return None; // recursive const-eval: not constant
    }
    CE_DEPTH.with(|d| d.set(depth + 1));
    // Hidden shape binds fold against the caller's frame, filtered to
    // the picked candidate's shape-bound formals.
    let binds = eval_hidden_binds(&f.hidden, shapes, env, funcs, extras, slots)?;
    let r = const_eval_func(f, &argv, &binds, env, funcs, extras);
    CE_DEPTH.with(|d| d.set(depth));
    ce_trace(name, f, &argv, &r);
    r
}

/// System functions with constant meaning (`$clog2`, `$const_idx`, VHDL
/// `type'val`/`type'pos`). `None` = not one of them (the caller tries user
/// functions); `Some(v)` = the fold result, `None` inside when the operand
/// is not constant.
fn ce_builtin(
    name: &str,
    args: &[Expr],
    env: &HashMap<String, Value>,
    funcs: &HashMap<String, Vec<FuncIr>>,
    extras: &HashMap<String, Vec<Rc<FuncIr>>>,
    mut slots: Option<&mut Vec<Value>>,
) -> Option<Option<Value>> {
    if (name == "$clog2" || name == "clog2") && args.len() == 1 {
        let v = ce_impl(&args[0], env, funcs, extras, slots.as_deref_mut())?;
        let n = v.to_u64()?;
        return Some(Some(Value::from_u64(
            32,
            (64 - n.leading_zeros()).max(1) as u64,
        )));
    }
    // `$const_idx(val, i, w)`: element `i` (MSB-first) of a flat array
    // value with element width `w` — array-constant indexing
    // (`C_VERSION(0)`, `const_arr(i).field`) and record-internal array
    // reads lower to this call. Same semantics as the simulator's
    // eval_call arm.
    if name == "$const_idx" && args.len() == 3 {
        let v = ce_impl(&args[0], env, funcs, extras, slots.as_deref_mut())?;
        let a = ce_impl(&args[1], env, funcs, extras, slots.as_deref_mut())?.to_u64()? as usize;
        let w = ce_impl(&args[2], env, funcs, extras, slots.as_deref_mut())?
            .to_u64()?
            .max(1) as usize;
        let total = v.width();
        return Some(Some(if total >= (a + 1) * w && a * w < total {
            v.slice(total - 1 - a * w, total - (a + 1) * w)
        } else {
            Value::x(w)
        }));
    }
    // VHDL `type'val(x)` / `type'pos(x)` — numeric identity (the 'val
    // result width is applied by a Resize at the lower site).
    if (name == "$val" || name == "$pos") && args.len() == 1 {
        return Some(ce_impl(&args[0], env, funcs, extras, slots));
    }
    None
}

/// Candidate function bodies for `name`: the directly visible table first,
/// else the design-wide package fallback — either/or, never merged. Within
/// a table, real bodies come before interface stubs (a stub const-evals to
/// X, which loops on forever — see the matching ordering in the simulator's
/// dispatch).
fn ce_candidates<'a>(
    name: &str,
    funcs: &'a HashMap<String, Vec<FuncIr>>,
    extras: &'a HashMap<String, Vec<Rc<FuncIr>>>,
) -> Option<Vec<&'a FuncIr>> {
    match funcs.get(&name.to_ascii_lowercase()) {
        Some(c) => {
            let mut v: Vec<&FuncIr> = c.iter().filter(|f| !is_stub_ir(f)).collect();
            v.extend(c.iter().filter(|f| is_stub_ir(f)));
            Some(v)
        }
        None => {
            let c = extras.get(&name.to_ascii_lowercase())?;
            let mut v: Vec<&FuncIr> = c
                .iter()
                .map(std::rc::Rc::as_ref)
                .filter(|f| !is_stub_ir(f))
                .collect();
            v.extend(c.iter().map(std::rc::Rc::as_ref).filter(|f| is_stub_ir(f)));
            Some(v)
        }
    }
}

/// Overload resolution: same-name candidates may exist (VHDL overloaded
/// subprograms, e.g. `elog2(integer)` vs `elog2(std_ulogic_vector)`).
/// Prefer the candidate whose formal count matches, then the one whose
/// formal widths fit the actual argument widths (exact match, or an
/// unconstrained/unknown formal width of 0 or 1). Without a fitting
/// candidate, the first same-arity one runs — matching the simulator's
/// runtime pick.
fn ce_pick<'a>(cands: &[&'a FuncIr], argv: &[Value]) -> Option<&'a FuncIr> {
    // Width fit is checked across ALL fitting candidates —
    // exact-arity and defaults-fit alike — exact widths before
    // wildcards: an unconstrained (w <= 1) formal must not absorb a
    // call an exact-width overload can take (a real-typed actual,
    // w64, must reach the real overload, not a same-name
    // std_ulogic_vector overload registered first).
    ce_pick_width(cands, argv, false)
        .or_else(|| ce_pick_width(cands, argv, true))
        // No candidate matched every actual width: score the rest by
        // formal fit, best first — a `(record_t, boolean, natural, …)`
        // overload stays ahead of a `(natural, string, string, …)`
        // namesake when a record actual arrives, though neither
        // matched exactly (a scalar actual folds to a width the
        // formal does not declare). A same-arity interface stub must
        // never win this scored fit against a real body: its formals
        // carry fallback widths and its body const-evals to X, so a
        // real-body call whose actual is nearer the stub's fallback
        // width would fold the constant to X (`expand("abc", 5)`
        // scored the w32 stub over the w8 real `string` formal). When
        // any real body is viable, stubs drop out of the pool; an
        // unelaborated interface (stubs only) keeps the scored pick.
        .or_else(|| ce_pick_scored(cands, argv))
        .or_else(|| ce_pick_arity(cands, argv))
        .or_else(|| ce_pick_defaults(cands, argv))
        .or_else(|| cands.first().copied())
}

/// A call whose actuals all match some candidate's declared widths (a
/// wildcard formal absorbs anything when `wild`).
fn ce_pick_width<'a>(cands: &[&'a FuncIr], argv: &[Value], wild: bool) -> Option<&'a FuncIr> {
    cands
        .iter()
        .find(|f| viable(f, argv) && width_ok(f, argv, wild))
        .copied()
}

/// Best formal-fit score among viable candidates; real bodies only when any
/// real body is viable.
fn ce_pick_scored<'a>(cands: &[&'a FuncIr], argv: &[Value]) -> Option<&'a FuncIr> {
    let has_real = cands.iter().any(|f| viable(f, argv) && !is_stub_ir(f));
    cands
        .iter()
        .filter(|f| viable(f, argv) && (has_real == !is_stub_ir(f)))
        .filter_map(|f| ce_overload_fit(f, argv).map(|c| (c, f)))
        .min_by_key(|(c, _)| *c)
        .map(|(_, f)| *f)
}

/// First candidate matching the actual count exactly.
fn ce_pick_arity<'a>(cands: &[&'a FuncIr], argv: &[Value]) -> Option<&'a FuncIr> {
    cands.iter().find(|f| f.args.len() == argv.len()).copied()
}

/// First candidate whose omitted trailing formals all carry defaults.
fn ce_pick_defaults<'a>(cands: &[&'a FuncIr], argv: &[Value]) -> Option<&'a FuncIr> {
    cands.iter().find(|f| fits_defaults(f, argv.len())).copied()
}

/// Interface-only stub: an empty body that const-evals to X.
fn is_stub_ir(f: &FuncIr) -> bool {
    match &f.body {
        Stmt::Null => true,
        Stmt::Block(ss) => ss.is_empty(),
        _ => false,
    }
}

/// A call may omit trailing actuals whose formals carry interface
/// defaults (`w : natural := 8`); the real body qualifies, the
/// interface-only spec stub does not.
fn fits_defaults(f: &FuncIr, n: usize) -> bool {
    f.args.len() > n
        && f.defaults
            .get(n..f.args.len())
            .is_some_and(|t| t.iter().all(|d| d.is_some()))
        && !matches!(&f.body, Stmt::Block(ss) if ss.is_empty())
        && !matches!(f.body, Stmt::Null)
}

/// Formal widths fit the actual argument widths (exact match, or a
/// wildcard formal of width 0/1 when `wild`).
fn width_ok(f: &FuncIr, argv: &[Value], wild: bool) -> bool {
    f.args
        .iter()
        .zip(argv.iter())
        .all(|((_, w, _), v)| *w == v.width() || (wild && *w <= 1))
}

/// Viability (exact arity or trailing defaults); checked per candidate.
fn viable(f: &FuncIr, argv: &[Value]) -> bool {
    f.args.len() == argv.len() || fits_defaults(f, argv.len())
}

/// Fill the omitted actuals from the recorded defaults.
fn ce_fill_defaults(f: &FuncIr, argv: &mut Vec<Value>) {
    for (j, d) in f.defaults.iter().enumerate().skip(argv.len()) {
        match d {
            Some(v) => {
                let w = f.args.get(j).map(|a| a.1).unwrap_or(v.width()).max(1);
                argv.push(v.clone().resize(w));
            }
            None => break,
        }
    }
}

/// `--trace-ce` log line for one picked overload.
fn ce_trace(name: &str, f: &FuncIr, argv: &[Value], r: &Option<Value>) {
    if crate::debug::TRACE_CE.load(std::sync::atomic::Ordering::Relaxed)
        && (name.starts_with("get_")
            || crate::debug::TRACE_CE_ALL.load(std::sync::atomic::Ordering::Relaxed))
    {
        eprintln!(
            "ce {name} arity={} argvw {:?} argv {:?} -> {:?} pickedw {:?} body {:.80?}",
            f.args.len(),
            argv.iter().map(|v| v.width()).collect::<Vec<_>>(),
            argv.iter().map(|v| v.to_u64()).collect::<Vec<_>>(),
            r.as_ref().map(|v| (v.width(), v.to_u64())),
            f.args.iter().map(|a| a.1).collect::<Vec<_>>(),
            f.body,
        );
    }
}

/// Score how well an overload's formals fit a call's actuals when no
/// candidate matched every width exactly (see the `Call` arm): lower fits
/// better, `None` disqualifies. A declared-numeric formal can never take a
/// multi-word array/record actual — widths beyond 64 bits are arrays or
/// records, not scalars — and other formals match by width distance, which
/// keeps a `(record_t, boolean, natural, …)` overload ahead of a
/// `(natural, string, string, …)` namesake when a record actual arrives.
/// Viability (exact arity or trailing defaults) is checked by the caller.
fn ce_overload_fit(f: &FuncIr, argv: &[Value]) -> Option<u64> {
    let mut cost = 0u64;
    for ((_, w, t), v) in f.args.iter().zip(argv.iter()) {
        let aw = v.width() as u64;
        if *w as u64 == aw {
            continue;
        }
        if matches!(
            t.as_str(),
            "natural" | "integer" | "positive" | "boolean" | "bit" | "character" | "real" | "time",
        ) {
            // A scalar formal cannot bind an array/record actual.
            if aw > 64 {
                return None;
            }
            cost += 1;
        } else {
            cost += aw.abs_diff(*w as u64);
        }
    }
    Some(cost)
}
