//! Const-eval of user function bodies: the statement interpreter ([`ce_stmt`]),
//! the [`CeFlow`] control-flow signals, and the frame builder
//! [`const_eval_func`] (slot 0 = return value, then args, then locals).
//! Supports the statement subset that appears in pure package functions
//! (block/if/case/assign/for/return). Split out of `verilog_lower.rs` to
//! keep it at its standards baseline; the expression side lives in
//! `const_eval.rs`, procedure calls in `ce_proc.rs`.

use std::collections::HashMap;
use std::rc::Rc;

use crate::ir::{Expr, FuncIr, ShapeBind, Stmt};
use crate::value::{Value, V1, VX};

use super::ce_proc::{ce_call_procedure, fill_hidden_slots, fold_dyn_locals};
use super::const_eval::ce_impl;

/// Control flow out of a const-evaluated function body.
#[derive(PartialEq, Debug)]
pub(crate) enum CeFlow {
    Continue,
    Return,
    Abort,
    /// `break` / VHDL `exit`: ends the innermost enclosing loop; consumed by
    /// the loop, which resumes normal flow after itself.
    LoopBreak,
    /// `continue` / VHDL `next`: skips to the loop's step; consumed by the
    /// loop, which then runs the step and the next iteration.
    LoopNext,
}

/// Const-evaluate a user function body: slot 0 is the return value, then
/// args, then locals.
pub(crate) fn const_eval_func(
    f: &FuncIr,
    argv: &[Value],
    binds: &[(usize, Value, Value)],
    env: &HashMap<String, Value>,
    funcs: &HashMap<String, Vec<FuncIr>>,
    extras: &HashMap<String, Vec<Rc<FuncIr>>>,
) -> Option<Value> {
    let mut slots: Vec<Value> = vec![Value::x(f.ret_width.max(1))];
    slots.extend(argv.iter().cloned());
    // Locals ride in slot order; the hidden shape slots are locals entries
    // (32-bit, init-less), so extend first and then overwrite the two slots
    // of each shape-bound formal from the call's binds.
    slots.extend(
        f.locals
            .iter()
            .map(|l| l.init.clone().unwrap_or_else(|| Value::x(l.width.max(1)))),
    );
    fill_hidden_slots(&f.hidden, binds, &mut slots);
    // Locals with formal-dependent constraints (`string(1 to size)`)
    // registered with a fallback width: re-fold the recorded bit-width
    // expression now that the actuals are in the slots (shared with the
    // procedure dispatcher). Unfoldable — the body will fold to X on use.
    if !fold_dyn_locals(&f.dyn_locals, &mut slots, env, funcs, extras) {
        return None;
    }
    let mut flow = CeFlow::Continue;
    ce_stmt(&f.body, &mut slots, env, funcs, extras, &mut flow);
    if flow == CeFlow::Abort
        // A `break`/`continue` that outlives every loop has no value.
        || flow == CeFlow::LoopBreak
        || flow == CeFlow::LoopNext
    {
        return None;
    }
    Some(slots.into_iter().next().unwrap_or(Value::x(1)))
}

/// Interpret one IR statement of a const-evaluated function body, writing
/// slot effects into `slots` and control flow into `flow`.
pub(crate) fn ce_stmt(
    s: &Stmt,
    slots: &mut Vec<Value>,
    env: &HashMap<String, Value>,
    funcs: &HashMap<String, Vec<FuncIr>>,
    extras: &HashMap<String, Vec<Rc<FuncIr>>>,
    flow: &mut CeFlow,
) {
    if *flow != CeFlow::Continue {
        return;
    }
    match s {
        Stmt::Null => {}
        Stmt::Spanned { inner, .. } => ce_stmt(inner, slots, env, funcs, extras, flow),
        Stmt::Block(ss) => {
            for x in ss {
                ce_stmt(x, slots, env, funcs, extras, flow);
                if *flow != CeFlow::Continue {
                    return;
                }
            }
        }
        Stmt::If { cond, then, els } => {
            ce_if(cond, then, els.as_deref(), slots, env, funcs, extras, flow)
        }
        Stmt::Assign { lhs, rhs, .. } => ce_assign(lhs, rhs, slots, env, funcs, extras, flow),
        Stmt::Return(e) => ce_return(e, slots, env, funcs, extras, flow),
        Stmt::Break => *flow = CeFlow::LoopBreak,
        Stmt::Continue => *flow = CeFlow::LoopNext,
        Stmt::For {
            init,
            cond,
            step,
            body,
        } => ce_for(init, cond, step, body, slots, env, funcs, extras, flow),
        Stmt::Case {
            expr,
            items,
            default,
            ..
        } => ce_case(
            expr,
            items,
            default.as_deref(),
            slots,
            env,
            funcs,
            extras,
            flow,
        ),
        Stmt::SysCall { name, args, shapes } => {
            ce_syscall(name, args, shapes, slots, env, funcs, extras, flow)
        }
        _ => *flow = CeFlow::Abort,
    }
}

#[allow(clippy::too_many_arguments)]
fn ce_if(
    cond: &Expr,
    then: &Stmt,
    els: Option<&Stmt>,
    slots: &mut Vec<Value>,
    env: &HashMap<String, Value>,
    funcs: &HashMap<String, Vec<FuncIr>>,
    extras: &HashMap<String, Vec<Rc<FuncIr>>>,
    flow: &mut CeFlow,
) {
    match ce_impl(cond, env, funcs, extras, Some(&mut *slots)).and_then(|v| v.to_u64()) {
        // An unknown condition is not TRUE: VHDL runs the sequence only
        // on TRUE (LRM 1076 §10.8) and Verilog takes the else branch on
        // x/z (IEEE 1800 §4.1) — the runtime behaves the same, and the
        // loop arms already terminate on an unknown condition, so
        // const-eval mirrors that instead of aborting the fold.
        Some(0) | None => {
            if let Some(els) = els {
                ce_stmt(els, slots, env, funcs, extras, flow);
            }
        }
        Some(_) => ce_stmt(then, slots, env, funcs, extras, flow),
    }
}

#[allow(clippy::too_many_arguments)]
fn ce_assign(
    lhs: &Expr,
    rhs: &Expr,
    slots: &mut Vec<Value>,
    env: &HashMap<String, Value>,
    funcs: &HashMap<String, Vec<FuncIr>>,
    extras: &HashMap<String, Vec<Rc<FuncIr>>>,
    flow: &mut CeFlow,
) {
    let v = ce_impl(rhs, env, funcs, extras, Some(&mut *slots));
    match (v, lhs) {
        (Some(v), Expr::Sig(i)) => {
            while slots.len() <= *i as usize {
                slots.push(Value::x(32));
            }
            slots[*i as usize] = v;
        }
        // `v(i) := x` / `v(hi downto lo) := x` on a local slot: splice
        // the rhs bits in at the index/slice offset, growing the slot
        // with X as needed (mirrors ce_impl's bit-index read of
        // Expr::Idx and msb/lsb read of Expr::Sel).
        (Some(v), Expr::Idx { e, i }) => {
            if !ce_splice_idx(e, i, &v, slots, env, funcs, extras) {
                *flow = CeFlow::Abort;
            }
        }
        (Some(v), Expr::Sel { e, msb, lsb }) => {
            if !ce_splice_sel(e, msb, lsb, &v, slots, env, funcs, extras) {
                *flow = CeFlow::Abort;
            }
        }
        _ => *flow = CeFlow::Abort,
    }
}

/// Splice `v`'s bits into slot `e[i .. i + w - 1]`; `false` aborts the fold.
fn ce_splice_idx(
    e: &Expr,
    idx: &Expr,
    v: &Value,
    slots: &mut Vec<Value>,
    env: &HashMap<String, Value>,
    funcs: &HashMap<String, Vec<FuncIr>>,
    extras: &HashMap<String, Vec<Rc<FuncIr>>>,
) -> bool {
    let ev = |x: &Expr, slots: &mut Vec<Value>| ce_impl(x, env, funcs, extras, Some(slots));
    let spliced = (|| {
        let i = ev(idx, slots)?.to_u64()? as usize;
        let Expr::Sig(s) = e else { return None };
        let s = *s as usize;
        while slots.len() <= s {
            slots.push(Value::x(32));
        }
        let w = v.width().max(1);
        // A bit index near 2^32 is a folded-negative index
        // (`v(i - 1)` with i = 0) read as u64 — abort the
        // fold instead of growing the slot by gigabytes.
        if i + w > (1 << 24) {
            return None::<()>;
        }
        if slots[s].bits.len() < i + w {
            let mut bits = slots[s].bits.clone();
            bits.resize(i + w, VX);
            slots[s] = Value { bits, real: false };
        }
        for b in 0..w {
            slots[s].set_bit(i + b, v.bit(b));
        }
        Some(())
    })();
    spliced.is_some()
}

/// Splice `v`'s bits into the slice `e[msb:lsb]`; `false` aborts the fold.
#[allow(clippy::too_many_arguments)]
fn ce_splice_sel(
    e: &Expr,
    msb: &Expr,
    lsb: &Expr,
    v: &Value,
    slots: &mut Vec<Value>,
    env: &HashMap<String, Value>,
    funcs: &HashMap<String, Vec<FuncIr>>,
    extras: &HashMap<String, Vec<Rc<FuncIr>>>,
) -> bool {
    let ev = |x: &Expr, slots: &mut Vec<Value>| ce_impl(x, env, funcs, extras, Some(slots));
    let spliced = (|| {
        let msb = ev(msb, slots)?.to_u64()? as usize;
        let lsb = ev(lsb, slots)?.to_u64()? as usize;
        let (s, off) = ce_sel_base(e, slots, env, funcs, extras)?;
        while slots.len() <= s {
            slots.push(Value::x(32));
        }
        let w = msb.saturating_sub(lsb) + 1;
        // Guard the slot growth: a slice beyond a few MB is
        // a misfolded index (a negative bound read as u64),
        // never a real constant — abort the fold instead of
        // gigabytes of X.
        if w > (1 << 24) || lsb + off > (1 << 24) {
            return None::<()>;
        }
        if slots[s].bits.len() < lsb + off + w {
            let mut bits = slots[s].bits.clone();
            bits.resize(lsb + off + w, VX);
            slots[s] = Value { bits, real: false };
        }
        for b in 0..w {
            slots[s].set_bit(lsb + off + b, v.bit(b));
        }
        Some(())
    })();
    spliced.is_some()
}

/// Walk Sel chains down to the base slot: a record field slice-assign
/// (`result.description(1 TO n)`) is Sel-of-Sel; each inner field select's
/// low bit offsets the splice target (record field offsets are static bit
/// ranges). Returns (slot id, accumulated lsb offset).
fn ce_sel_base(
    e: &Expr,
    slots: &mut Vec<Value>,
    env: &HashMap<String, Value>,
    funcs: &HashMap<String, Vec<FuncIr>>,
    extras: &HashMap<String, Vec<Rc<FuncIr>>>,
) -> Option<(usize, usize)> {
    let ev = |x: &Expr, slots: &mut Vec<Value>| ce_impl(x, env, funcs, extras, Some(slots));
    let mut off = 0usize;
    let mut node = e;
    let s = loop {
        match node {
            Expr::Sig(s) => break *s as usize,
            Expr::Sel {
                e: inner, msb, lsb, ..
            } => {
                off += ev(lsb, slots)?.to_u64()? as usize;
                let _ = ev(msb, slots)?.to_u64()?;
                node = &**inner;
            }
            _ => return None,
        }
    };
    Some((s, off))
}

#[allow(clippy::too_many_arguments)]
fn ce_return(
    e: &Expr,
    slots: &mut Vec<Value>,
    env: &HashMap<String, Value>,
    funcs: &HashMap<String, Vec<FuncIr>>,
    extras: &HashMap<String, Vec<Rc<FuncIr>>>,
    flow: &mut CeFlow,
) {
    match ce_impl(e, env, funcs, extras, Some(&mut *slots)) {
        Some(v) => {
            slots[0] = v;
            *flow = CeFlow::Return;
        }
        None => *flow = CeFlow::Abort,
    }
}

#[allow(clippy::too_many_arguments)]
fn ce_for(
    init: &Stmt,
    cond: &Expr,
    step: &Stmt,
    body: &Stmt,
    slots: &mut Vec<Value>,
    env: &HashMap<String, Value>,
    funcs: &HashMap<String, Vec<FuncIr>>,
    extras: &HashMap<String, Vec<Rc<FuncIr>>>,
    flow: &mut CeFlow,
) {
    ce_stmt(init, slots, env, funcs, extras, flow);
    let mut guard = 0u32;
    loop {
        if *flow != CeFlow::Continue {
            return;
        }
        match ce_impl(cond, env, funcs, extras, Some(&mut *slots)).and_then(|v| v.to_u64()) {
            Some(n) if n != 0 => {
                ce_stmt(body, slots, env, funcs, extras, flow);
                match *flow {
                    // The innermost loop consumes break/continue so an
                    // enclosing loop (or the function epilogue) sees
                    // normal flow; Return/Abort still propagate.
                    CeFlow::LoopBreak => {
                        *flow = CeFlow::Continue;
                        return;
                    }
                    CeFlow::LoopNext => *flow = CeFlow::Continue,
                    CeFlow::Continue => {}
                    CeFlow::Return | CeFlow::Abort => return,
                }
                ce_stmt(step, slots, env, funcs, extras, flow);
                guard += 1;
                if guard > 100_000 {
                    *flow = CeFlow::Abort;
                    return;
                }
            }
            _ => return,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn ce_case(
    expr: &Expr,
    items: &[(Vec<Expr>, Stmt)],
    default: Option<&Stmt>,
    slots: &mut Vec<Value>,
    env: &HashMap<String, Value>,
    funcs: &HashMap<String, Vec<FuncIr>>,
    extras: &HashMap<String, Vec<Rc<FuncIr>>>,
    flow: &mut CeFlow,
) {
    let v = match ce_impl(expr, env, funcs, extras, Some(&mut *slots)) {
        Some(v) => v,
        None => {
            *flow = CeFlow::Abort;
            return;
        }
    };
    for (choices, body) in items {
        for ch in choices {
            if ce_impl(ch, env, funcs, extras, Some(&mut *slots)) == Some(v.clone()) {
                ce_stmt(body, slots, env, funcs, extras, flow);
                return;
            }
        }
    }
    if let Some(d) = default {
        ce_stmt(d, slots, env, funcs, extras, flow);
    }
}

#[allow(clippy::too_many_arguments)]
fn ce_syscall(
    name: &str,
    args: &[Expr],
    shapes: &[ShapeBind],
    slots: &mut Vec<Value>,
    env: &HashMap<String, Value>,
    funcs: &HashMap<String, Vec<FuncIr>>,
    extras: &HashMap<String, Vec<Rc<FuncIr>>>,
    flow: &mut CeFlow,
) {
    // VHDL `report`/`assert` inside a constant initializer: a note or
    // warning only annotates elaboration, so const-eval continues past
    // it (a `log2` that warns on non-power-of-2 input must still
    // produce a value). Error/failure — and a failing `assert`, whose
    // default severity is error (LRM 1076 §10.8) — mean the
    // initializer would fail at elaboration, so it folds to X. Other
    // syscalls (file I/O, $display) have no constant meaning.
    match name {
        "report" => {
            if matches!(
                ce_severity(args, 1, slots, env, funcs, extras).as_str(),
                "error" | "failure"
            ) {
                *flow = CeFlow::Abort;
            }
        }
        "assert" => {
            let ok = args
                .first()
                .and_then(|a| ce_impl(a, env, funcs, extras, Some(&mut *slots)))
                .and_then(|v| v.to_u64())
                .map(|n| n != 0)
                .unwrap_or(true);
            if !ok
                && !matches!(
                    ce_severity(args, 2, slots, env, funcs, extras).as_str(),
                    "note" | "warning"
                )
            {
                *flow = CeFlow::Abort;
            }
        }
        _ => {
            // A user procedure called as a statement: run its body
            // on a fresh frame and copy inout/out formals back (see
            // `ce_proc`). Builtin syscalls and unresolvable names
            // have no constant meaning and abort the fold.
            if !ce_call_procedure(name, args, shapes, slots, env, funcs, extras) {
                *flow = CeFlow::Abort;
            }
        }
    }
}

/// Decode a report/assert severity string argument (`args[i]`) to lowercase.
fn ce_severity(
    args: &[Expr],
    i: usize,
    slots: &mut Vec<Value>,
    env: &HashMap<String, Value>,
    funcs: &HashMap<String, Vec<FuncIr>>,
    extras: &HashMap<String, Vec<Rc<FuncIr>>>,
) -> String {
    args.get(i)
        .and_then(|a| ce_impl(a, env, funcs, extras, Some(&mut *slots)))
        .map(|v| {
            v.bits
                .chunks(8)
                .rev()
                .filter_map(|c| {
                    let mut b = 0u8;
                    for (j, &bit) in c.iter().enumerate() {
                        if bit == V1 {
                            b |= 1 << j;
                        }
                    }
                    (b != 0).then_some(b as char)
                })
                .collect::<String>()
                .to_ascii_lowercase()
        })
        .unwrap_or_default()
}
