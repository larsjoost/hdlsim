//! Constant expression evaluation over the lowered IR (`Expr` → `Value`).
//! Split out of `verilog_lower.rs` to keep it at its standards baseline:
//! the public entry points ([`const_eval`], [`const_eval_f`],
//! [`const_eval_f_with_pkgs`]), the expression interpreter [`ce_impl`] with
//! one helper per expression form, and the `Expr::Call` dispatch into user
//! function bodies (overload resolution, depth guard, trace). Statement
//! interpretation lives in `ce_func.rs`, procedure calls in `ce_proc.rs`.

use std::collections::HashMap;
use std::rc::Rc;

use crate::ir::{Expr, FuncIr};
use crate::value::{BinOp, UnOp, Value, V0, V1};

use super::ce_call::ce_call;

/// Constant-evaluate an expression with an environment of named constants.
pub fn const_eval(e: &Expr, env: &HashMap<String, Value>) -> Option<Value> {
    let no_funcs: HashMap<String, Vec<FuncIr>> = HashMap::new();
    let no_fallback: HashMap<String, Vec<std::rc::Rc<FuncIr>>> = HashMap::new();
    ce_impl(e, env, &no_funcs, &no_fallback, None)
}

/// Like [`const_eval`], but user function bodies (`funcs`, e.g. VHDL package
/// functions visible through `use`) are const-evaluated for `Expr::Call` —
/// needed for constant initializers like `constant c : natural := f(1, 2);`.
pub fn const_eval_f(
    e: &Expr,
    env: &HashMap<String, Value>,
    funcs: &HashMap<String, Vec<FuncIr>>,
) -> Option<Value> {
    let no_fallback: HashMap<String, Vec<std::rc::Rc<FuncIr>>> = HashMap::new();
    ce_impl(e, env, funcs, &no_fallback, None)
}

/// Like [`const_eval_f`], but a call no visible function (`funcs`) provides
/// falls back to `pkg_funcs` — the function bodies of all known packages.
/// VHDL visibility limits the directly merged table (`use p.f;` merges only
/// `f`), yet `f`'s body may legally call a package-internal sibling (visible
/// at the body's declaration site), so nested const-eval needs the wider
/// table to avoid poisoning the whole constant chain with X.
pub fn const_eval_f_with_pkgs(
    e: &Expr,
    env: &HashMap<String, Value>,
    funcs: &HashMap<String, Vec<FuncIr>>,
    pkg_funcs: &HashMap<String, Vec<std::rc::Rc<FuncIr>>>,
) -> Option<Value> {
    ce_impl(e, env, funcs, pkg_funcs, None)
}

thread_local! {
    /// Const-eval call depth: recursive or mutually recursive functions
    /// (e.g. OSVVM's MetaMatch) must abort instead of overflowing the stack.
    pub(crate) static CE_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

pub(crate) const CE_MAX_DEPTH: u32 = 64;

/// Whether a const-evaluated expression is real-typed (mirrors the
/// simulator's `expr_real`): literals and arithmetic propagate realness.
fn ce_real(e: &Expr) -> bool {
    match e {
        Expr::Real(_) => true,
        Expr::Bin {
            op: BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod,
            l,
            r,
        } => ce_real(l) || ce_real(r),
        Expr::Bin { .. } => false,
        Expr::Un {
            op: UnOp::Neg | UnOp::Pos,
            e,
        } => ce_real(e),
        Expr::Un { .. } => false,
        Expr::Resize { e, .. } | Expr::Signed(e) => ce_real(e),
        Expr::Cond { c, t, e } => ce_real(c) || ce_real(t) || ce_real(e),
        _ => false,
    }
}

/// Const-eval one operand of a real-typed binop as f64.
fn ce_f64(v: &Value) -> Option<f64> {
    v.to_f64().or_else(|| v.to_u64().map(|n| n as f64))
}

/// Whether a const-evaluated expression is signed (mirrors the simulator's
/// `expr_signed`): `Signed`/`Resize{signed}` markers carry it; literal and
/// env operands are unsigned.
fn ce_signed(e: &Expr) -> bool {
    match e {
        Expr::Signed(_) => true,
        Expr::Resize { signed, .. } => *signed,
        Expr::Bin { op, l, r } => {
            if matches!(
                op,
                BinOp::Shl | BinOp::Shr | BinOp::Sar | BinOp::Rol | BinOp::Ror
            ) {
                ce_signed(l)
            } else {
                ce_signed(l) && ce_signed(r)
            }
        }
        Expr::Un { e, .. } => ce_signed(e),
        _ => false,
    }
}

/// Sign-extended integer value for ordered comparisons, or `None` when the
/// value holds metavariables. Mirrors the kernel's signed comparison.
fn ce_to_i(v: &Value, w: usize) -> Option<i128> {
    if !v.is_known() {
        return None;
    }
    let mut x: i128 = 0;
    for (i, &b) in v.bits.iter().enumerate() {
        if b == V1 {
            x |= 1 << i;
        }
    }
    if w < 128 && v.bit(w - 1) == V1 {
        x |= -1i128 << w; // sign bit
    }
    Some(x)
}

/// Interpret one IR expression to a constant, or `None` when it is not
/// constant. `Some(slots)` while interpreting a function body:
/// `Expr::Sig(i)` then reads slot `i` (slot 0 = return value, then args,
/// then locals) instead of failing.
pub(crate) fn ce_impl(
    e: &Expr,
    env: &HashMap<String, Value>,
    funcs: &HashMap<String, Vec<FuncIr>>,
    extras: &HashMap<String, Vec<Rc<FuncIr>>>,
    mut slots: Option<&mut Vec<Value>>,
) -> Option<Value> {
    match e {
        Expr::Rising(_) | Expr::Falling(_) | Expr::Prev(_) => None,
        Expr::ToInt(inner) => ce_to_int(inner, env, funcs, extras, slots.as_deref_mut()),
        Expr::ToString { e: inner, .. } => {
            ce_to_string(inner, env, funcs, extras, slots.as_deref_mut())
        }
        Expr::Lit(v) => Some(v.clone()),
        Expr::Real(x) => Some(Value::from_f64(*x)),
        Expr::Sig(i) => ce_sig_slot(*i, slots.as_deref_mut()),
        Expr::Bin { op, l, r } => ce_bin(op, l, r, env, funcs, extras, slots.as_deref_mut()),
        Expr::Un { op, e: inner } => ce_un(op, inner, env, funcs, extras, slots.as_deref_mut()),
        Expr::Cond { c, t, e: inner } => {
            ce_cond(c, t, inner, env, funcs, extras, slots.as_deref_mut())
        }
        Expr::Concat(parts) => ce_concat(parts, env, funcs, extras, slots.as_deref_mut()),
        Expr::Stream { e: inner, slice } => {
            ce_stream(inner, *slice, env, funcs, extras, slots.as_deref_mut())
        }
        Expr::Replicate { count, e: inner } => {
            ce_replicate(count, inner, env, funcs, extras, slots.as_deref_mut())
        }
        Expr::Sel { e: inner, msb, lsb } => {
            ce_sel(inner, msb, lsb, env, funcs, extras, slots.as_deref_mut())
        }
        Expr::Idx { e: inner, i } => ce_idx(inner, i, env, funcs, extras, slots.as_deref_mut()),
        Expr::Resize {
            e: inner,
            width,
            signed,
        } => ce_resize(
            inner,
            *width,
            *signed,
            env,
            funcs,
            extras,
            slots.as_deref_mut(),
        ),
        Expr::Signed(inner) => ce_impl(inner, env, funcs, extras, slots.as_deref_mut()),
        Expr::SigAttr { sig, attr } => ce_sig_attr(sig, attr, slots),
        Expr::Call { name, args, shapes } => ce_call(name, args, shapes, env, funcs, extras, slots),
    }
}

/// `to_integer(x)` in a constant initializer.
fn ce_to_int(
    e: &Expr,
    env: &HashMap<String, Value>,
    funcs: &HashMap<String, Vec<FuncIr>>,
    extras: &HashMap<String, Vec<Rc<FuncIr>>>,
    slots: Option<&mut Vec<Value>>,
) -> Option<Value> {
    let v = ce_impl(e, env, funcs, extras, slots)?;
    // Realness is a value property for slot reads: a formal/variable
    // slot holding a real (f64 bits, `real: true`) must round as a
    // number, not truncate its bit pattern to an integer (a w64 real
    // read via `to_u64` yields e.g. 0x1E1E1E1E for 1/17 — the low
    // half of the f64 bits).
    if ce_real(e) || v.real {
        let f = ce_f64(&v)?;
        return Some(Value::from_u64(32, f.round() as u64));
    }
    v.to_u64().map(|n| Value::from_u64(32, n))
}

/// `T'image(x)` in a constant initializer (e.g. integer'image(42)).
fn ce_to_string(
    e: &Expr,
    env: &HashMap<String, Value>,
    funcs: &HashMap<String, Vec<FuncIr>>,
    extras: &HashMap<String, Vec<Rc<FuncIr>>>,
    slots: Option<&mut Vec<Value>>,
) -> Option<Value> {
    let v = ce_impl(e, env, funcs, extras, slots)?;
    let s = if ce_real(e) || v.real {
        ce_f64(&v).map(|f| format!("{}", f))?
    } else if ce_signed(e) {
        // Signed integer image: sign-extend from the value's own
        // width (`integer'image(-1)` is "-1", not 4294967295).
        v.to_i64().map(|n| n.to_string())?
    } else {
        v.to_u64().map(|n| n.to_string())?
    };
    Some(crate::vhdl_lower::string_value(&s))
}

/// Read slot `i` of the interpreted frame, growing it with X as needed.
fn ce_sig_slot(i: crate::ir::SigId, slots: Option<&mut Vec<Value>>) -> Option<Value> {
    match slots {
        Some(s) => {
            while s.len() <= i as usize {
                s.push(Value::x(32));
            }
            Some(s[i as usize].clone())
        }
        None => None,
    }
}

fn ce_bin(
    op: &BinOp,
    l: &Expr,
    r: &Expr,
    env: &HashMap<String, Value>,
    funcs: &HashMap<String, Vec<FuncIr>>,
    extras: &HashMap<String, Vec<Rc<FuncIr>>>,
    mut slots: Option<&mut Vec<Value>>,
) -> Option<Value> {
    let lv = ce_impl(l, env, funcs, extras, slots.as_deref_mut())?;
    let rv = ce_impl(r, env, funcs, extras, slots)?;
    // Realness is a value property for slot reads (see ToInt): a
    // `x * y` of two slot-held reals must multiply numbers, not the
    // f64 bit patterns.
    if ce_real(l) || ce_real(r) || lv.real || rv.real {
        return ce_bin_real(*op, &lv, &rv);
    }
    // Const-eval convention (same as the BMC encoding): div/mod by
    // zero yields 0, so a default like `depth = N/width` with
    // `width=0` still folds instead of failing the range check.
    if matches!(op, BinOp::Div | BinOp::Mod) && rv.to_u64() == Some(0) {
        return Some(Value::from_u64(1, 0));
    }
    // Ordered comparison with a signed operand: sign-extend both
    // to the wider width first (`C_NEG >= 0` with `C_NEG = -1`
    // must fold to false, not 4294967295 >= 0).
    if matches!(op, BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge) && (ce_signed(l) || ce_signed(r))
    {
        return ce_bin_signed_cmp(*op, &lv, &rv);
    }
    Some(lv.binop(*op, &rv))
}

/// Real-typed binop: both operands as f64 numbers.
fn ce_bin_real(op: BinOp, lv: &Value, rv: &Value) -> Option<Value> {
    let a = ce_f64(lv);
    let b = ce_f64(rv);
    match (a, b) {
        (Some(x), Some(y)) => Some(match op {
            BinOp::Add => Value::from_f64(x + y),
            BinOp::Sub => Value::from_f64(x - y),
            BinOp::Mul => Value::from_f64(x * y),
            BinOp::Div => Value::from_f64(x / y),
            BinOp::Mod => Value::from_f64(x % y),
            BinOp::Pow => Value::from_f64(x.powf(y)),
            BinOp::Lt => Value::from_u64(1, (x < y) as u64),
            BinOp::Le => Value::from_u64(1, (x <= y) as u64),
            BinOp::Gt => Value::from_u64(1, (x > y) as u64),
            BinOp::Ge => Value::from_u64(1, (x >= y) as u64),
            BinOp::Eq => Value::from_u64(1, (x == y) as u64),
            BinOp::Ne => Value::from_u64(1, (x != y) as u64),
            _ => return None,
        }),
        _ => None,
    }
}

/// Ordered comparison with a signed operand: sign-extend both to the wider
/// width, then compare as i128.
fn ce_bin_signed_cmp(op: BinOp, lv: &Value, rv: &Value) -> Option<Value> {
    let w = lv.width().max(rv.width()).max(1);
    match (ce_to_i(lv, w), ce_to_i(rv, w)) {
        (Some(a), Some(b)) => Some(Value::from_u64(
            1,
            match op {
                BinOp::Lt => a < b,
                BinOp::Le => a <= b,
                BinOp::Gt => a > b,
                _ => a >= b,
            } as u64,
        )),
        _ => None,
    }
}

fn ce_un(
    op: &UnOp,
    e: &Expr,
    env: &HashMap<String, Value>,
    funcs: &HashMap<String, Vec<FuncIr>>,
    extras: &HashMap<String, Vec<Rc<FuncIr>>>,
    slots: Option<&mut Vec<Value>>,
) -> Option<Value> {
    let v = ce_impl(e, env, funcs, extras, slots)?;
    if ce_real(e) || v.real {
        return match (op, ce_f64(&v)) {
            (UnOp::Neg, Some(x)) => Some(Value::from_f64(-x)),
            (UnOp::Pos, Some(x)) => Some(Value::from_f64(x)),
            (UnOp::Abs, Some(x)) => Some(Value::from_f64(x.abs())),
            _ => None,
        };
    }
    Some(v.unop(*op))
}

fn ce_cond(
    c: &Expr,
    t: &Expr,
    e: &Expr,
    env: &HashMap<String, Value>,
    funcs: &HashMap<String, Vec<FuncIr>>,
    extras: &HashMap<String, Vec<Rc<FuncIr>>>,
    mut slots: Option<&mut Vec<Value>>,
) -> Option<Value> {
    if ce_impl(c, env, funcs, extras, slots.as_deref_mut())?.to_u64()? != 0 {
        ce_impl(t, env, funcs, extras, slots.as_deref_mut())
    } else {
        ce_impl(e, env, funcs, extras, slots)
    }
}

fn ce_concat(
    parts: &[Expr],
    env: &HashMap<String, Value>,
    funcs: &HashMap<String, Vec<FuncIr>>,
    extras: &HashMap<String, Vec<Rc<FuncIr>>>,
    mut slots: Option<&mut Vec<Value>>,
) -> Option<Value> {
    let mut vals = Vec::with_capacity(parts.len());
    for p in parts {
        vals.push(ce_impl(p, env, funcs, extras, slots.as_deref_mut())?);
    }
    Some(Value::concat(&vals))
}

fn ce_stream(
    e: &Expr,
    slice: u32,
    env: &HashMap<String, Value>,
    funcs: &HashMap<String, Vec<FuncIr>>,
    extras: &HashMap<String, Vec<Rc<FuncIr>>>,
    slots: Option<&mut Vec<Value>>,
) -> Option<Value> {
    let v = ce_impl(e, env, funcs, extras, slots)?;
    // A streamed constant folds to the streamed value (the `>>` identity
    // never reaches the IR — the lowerer folds it away).
    Some(super::stream_reverse(&v, slice))
}

fn ce_replicate(
    count: &Expr,
    e: &Expr,
    env: &HashMap<String, Value>,
    funcs: &HashMap<String, Vec<FuncIr>>,
    extras: &HashMap<String, Vec<Rc<FuncIr>>>,
    mut slots: Option<&mut Vec<Value>>,
) -> Option<Value> {
    let n = ce_impl(count, env, funcs, extras, slots.as_deref_mut())?.to_u64()? as usize;
    // A replication count beyond a few MB of bits is a misfolded
    // count, never a constant — abort the fold.
    if n > (1 << 21) {
        return None;
    }
    let v = ce_impl(e, env, funcs, extras, slots)?;
    Some(Value::concat(&vec![v; n]))
}

fn ce_sel(
    e: &Expr,
    msb: &Expr,
    lsb: &Expr,
    env: &HashMap<String, Value>,
    funcs: &HashMap<String, Vec<FuncIr>>,
    extras: &HashMap<String, Vec<Rc<FuncIr>>>,
    mut slots: Option<&mut Vec<Value>>,
) -> Option<Value> {
    let v = ce_impl(e, env, funcs, extras, slots.as_deref_mut())?;
    let msb = ce_impl(msb, env, funcs, extras, slots.as_deref_mut())?.to_u64()? as usize;
    let lsb = ce_impl(lsb, env, funcs, extras, slots)?.to_u64()? as usize;
    Some(v.slice(msb, lsb))
}

fn ce_idx(
    e: &Expr,
    i: &Expr,
    env: &HashMap<String, Value>,
    funcs: &HashMap<String, Vec<FuncIr>>,
    extras: &HashMap<String, Vec<Rc<FuncIr>>>,
    mut slots: Option<&mut Vec<Value>>,
) -> Option<Value> {
    let v = ce_impl(e, env, funcs, extras, slots.as_deref_mut())?;
    let i = ce_impl(i, env, funcs, extras, slots)?.to_u64()? as usize;
    Some(Value::from_bit(v.bit(i)))
}

fn ce_resize(
    e: &Expr,
    width: usize,
    signed: bool,
    env: &HashMap<String, Value>,
    funcs: &HashMap<String, Vec<FuncIr>>,
    extras: &HashMap<String, Vec<Rc<FuncIr>>>,
    slots: Option<&mut Vec<Value>>,
) -> Option<Value> {
    let v = ce_impl(e, env, funcs, extras, slots)?;
    if signed && v.width() < width {
        let mut bits = vec![if v.bit(v.width() - 1) == V1 { V1 } else { V0 }; width];
        bits[..v.width()].copy_from_slice(&v.bits);
        Some(Value { bits, real: false })
    } else {
        Some(v.resize(width))
    }
}

/// `sig'length` / `'high` / … of a function-local slot: read the bound
/// value's width (same rule as the runtime kernel).
fn ce_sig_attr(
    sig: &crate::ir::SigId,
    attr: &'static str,
    slots: Option<&mut Vec<Value>>,
) -> Option<Value> {
    // Function-local slots share the id space with flat signals; a
    // formal's `sig'length` / `'range` inside a const-evaluated
    // function body reads the bound value's width (same rule as the
    // runtime kernel). Signals proper stay unevaluated here — the
    // runtime resolves them after elaboration.
    let s = *sig as usize;
    let w = slots
        .as_deref()
        .and_then(|l| l.get(s))
        .map(|v| v.width().max(1))?;
    let v = match attr {
        "length" => w,
        "high" | "left" => w.saturating_sub(1),
        _ => 0, // low | right
    };
    Some(Value::from_u64(32, v as u64))
}
