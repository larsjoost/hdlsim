//! Symbolic expression evaluation over CNF literals: the `eval` dispatcher,
//! operator tables, and user-function inlining. Split out of `bmc.rs`
//! (statements live in `exec`, constant folding in `const_fold`).

use super::bitvec::{
    add_bits, and_bits, and_vec, divmod_bits, eq_bits, extend, ge_bits, lt_bits, mul_bits, mux_vec,
    not_bits, or_bits, or_vec, resize_bits, shift_bits, sub_bits, xor_vec, ShiftDir,
};
use super::builder::Builder;
use super::exec::{Exec, Val};
use crate::sat::{neg, Lit};
use hdl_ir::elab::{FlatFunc, FlatSigId};
use hdl_ir::value::{Value, V1};
use hdl_ir::{BinOp, Expr, ShapeBind, UnOp};
use std::collections::HashMap;

impl<'e, 'a> Exec<'e, 'a> {
    pub(super) fn eval(&mut self, e: &Expr) -> Result<Val, String> {
        match e {
            Expr::SigAttr { .. } => Err(
                "prove: runtime sig'length/'high on unconstrained ports unsupported".to_string(),
            ),
            Expr::Real(_) => {
                Err("prove: real-typed expressions are unsupported in BMC".to_string())
            }
            Expr::Lit(v) => self.eval_lit(v),
            Expr::Sig(s) => self.eval_sig(*s),
            Expr::Bin { op, l, r } => self.eval_bin(*op, l, r),
            Expr::Un { op, e } => self.eval_un(*op, e),
            Expr::Cond { c, t, e } => self.eval_cond(c, t, e),
            Expr::Call { name, args, shapes } => self.eval_call(name, args, shapes),
            Expr::Concat(parts) => self.eval_concat(parts),
            Expr::Stream { .. } => {
                Err("prove: SV streaming operator is unsupported in BMC".to_string())
            }
            Expr::Replicate { count, e } => self.eval_replicate(count, e),
            Expr::Sel { e, msb, lsb } => self.eval_sel(e, msb, lsb),
            Expr::Idx { e, i } => self.eval_idx(e, i),
            // Every BMC step is a clock edge: rising_edge is true, falling false.
            Expr::Rising(_) => Ok(Val {
                bits: vec![self.enc.b.true_lit()],
                signed: false,
            }),
            Expr::Falling(_) => Ok(Val {
                bits: vec![self.enc.b.false_lit()],
                signed: false,
            }),
            Expr::Prev(_) => Err("prove: 'last_value (Prev) unsupported".to_string()),
            Expr::ToInt(e) => self.eval_to_int(e),
            Expr::ToString { .. } => Err("prove: string conversion is not supported".to_string()),
            Expr::Resize { e, width, signed } => self.eval_resize(e, *width, *signed),
            Expr::Signed(e) => {
                let mut v = self.eval(e)?;
                v.signed = true;
                Ok(v)
            }
        }
    }

    /// A literal: 2-valued, so X/Z/… bits encode as 0.
    fn eval_lit(&self, v: &Value) -> Result<Val, String> {
        Ok(Val {
            bits: v
                .bits
                .iter()
                .map(|&b| self.enc.b.const_lit(b == V1))
                .collect(),
            signed: false,
        })
    }

    fn eval_sig(&self, s: FlatSigId) -> Result<Val, String> {
        let signed = self.enc.flat.signals[s as usize].signed;
        Ok(Val {
            bits: self.val(s)?,
            signed,
        })
    }

    fn eval_bin(&mut self, op: BinOp, l: &Expr, r: &Expr) -> Result<Val, String> {
        let lv = self.eval(l)?;
        let rv = self.eval(r)?;
        let w = lv.bits.len().max(rv.bits.len()).max(1);
        let signed = lv.signed || rv.signed;
        let b = &mut self.enc.b;
        let l2 = extend(b, &lv.bits, w, signed);
        let r2 = extend(b, &rv.bits, w, signed);
        // Matching operators have don't-care semantics that are not
        // 2-valued; reject rather than give a wrong answer.
        if matches!(
            op,
            BinOp::EqM | BinOp::NeM | BinOp::LtM | BinOp::LeM | BinOp::GtM | BinOp::GeM
        ) {
            return Err("prove: matching operators (?=, ?/=, ...) unsupported".to_string());
        }
        let bits = eval_bin_op(b, op, &l2, &r2, &rv.bits, w, signed)?;
        Ok(Val { bits, signed })
    }

    fn eval_un(&mut self, op: UnOp, e: &Expr) -> Result<Val, String> {
        let v = self.eval(e)?;
        let b = &mut self.enc.b;
        let w = v.bits.len().max(1);
        let bits = match op {
            UnOp::Not => vec![neg(or_bits(b, &v.bits))],
            UnOp::BNot => not_bits(&v.bits),
            UnOp::Neg => sub_bits(b, &vec![b.false_lit(); w], &v.bits, w),
            UnOp::Pos => v.bits.clone(),
            // `abs` in BMC: approximated as identity (sign handling is
            // not modeled symbolically).
            UnOp::Abs => v.bits.clone(),
            UnOp::RedAnd => vec![and_bits(b, &v.bits)],
            UnOp::RedOr => vec![or_bits(b, &v.bits)],
            UnOp::RedXor => {
                let mut acc = b.false_lit();
                for &x in &v.bits {
                    acc = b.xor_lit(acc, x);
                }
                vec![acc]
            }
            UnOp::RedNand => vec![neg(and_bits(b, &v.bits))],
            UnOp::RedNor => vec![neg(or_bits(b, &v.bits))],
            UnOp::RedXnor => {
                let mut acc = b.false_lit();
                for &x in &v.bits {
                    acc = b.xor_lit(acc, x);
                }
                vec![neg(acc)]
            }
        };
        Ok(Val {
            bits,
            signed: false,
        })
    }

    fn eval_cond(&mut self, c: &Expr, t: &Expr, e: &Expr) -> Result<Val, String> {
        let cv = self.eval(c)?;
        let tv = self.eval(t)?;
        let ev = self.eval(e)?;
        let w = tv.bits.len().max(ev.bits.len()).max(1);
        let b = &mut self.enc.b;
        let t2 = extend(b, &tv.bits, w, tv.signed);
        let e2 = extend(b, &ev.bits, w, ev.signed);
        let cond = or_bits(b, &cv.bits);
        Ok(Val {
            bits: mux_vec(b, cond, &t2, &e2),
            signed: tv.signed || ev.signed,
        })
    }

    fn eval_concat(&mut self, parts: &[Expr]) -> Result<Val, String> {
        let mut vals = Vec::new();
        for p in parts {
            vals.push(self.eval(p)?);
        }
        // parts[0] is most significant: its bits go to the high end.
        let mut bits = Vec::new();
        for v in vals.iter().rev() {
            bits.extend_from_slice(&v.bits);
        }
        Ok(Val {
            bits,
            signed: false,
        })
    }

    fn eval_replicate(&mut self, count: &Expr, e: &Expr) -> Result<Val, String> {
        let n = self
            .const_u64(count)
            .ok_or("prove: non-constant replication count is not supported")?
            as usize;
        let v = self.eval(e)?;
        let mut bits = Vec::new();
        for _ in 0..n.min(4096) {
            bits.extend_from_slice(&v.bits);
        }
        Ok(Val {
            bits,
            signed: v.signed,
        })
    }

    fn eval_sel(&mut self, e: &Expr, msb: &Expr, lsb: &Expr) -> Result<Val, String> {
        let v = self.eval(e)?;
        let m = self
            .const_u64(msb)
            .ok_or("prove: non-constant part-select is not supported")? as usize;
        let l = self
            .const_u64(lsb)
            .ok_or("prove: non-constant part-select is not supported")? as usize;
        let len = v.bits.len();
        let msb = m.min(len.saturating_sub(1));
        let lsb = l.min(msb);
        Ok(Val {
            bits: v.bits[lsb..=msb].to_vec(),
            signed: v.signed,
        })
    }

    fn eval_idx(&mut self, e: &Expr, i: &Expr) -> Result<Val, String> {
        let a = self
            .const_u64(i)
            .ok_or("prove: non-constant bit index is not supported")? as usize;
        if let Expr::Sig(s) = e {
            if self.enc.flat.signals[*s as usize].mem.is_some() {
                return Err("prove: memory read not supported".to_string());
            }
        }
        let v = self.eval(e)?;
        let bit = if a < v.bits.len() {
            v.bits[a]
        } else {
            self.enc.b.false_lit()
        };
        Ok(Val {
            bits: vec![bit],
            signed: false,
        })
    }

    fn eval_to_int(&mut self, e: &Expr) -> Result<Val, String> {
        let v = self.eval(e)?;
        let b = &self.enc.b;
        Ok(Val {
            bits: extend(b, &v.bits, 32, false),
            signed: false,
        })
    }

    fn eval_resize(&mut self, e: &Expr, width: usize, signed: bool) -> Result<Val, String> {
        let v = self.eval(e)?;
        let b = &self.enc.b;
        Ok(Val {
            bits: extend(b, &v.bits, width, signed),
            signed,
        })
    }

    fn eval_call(
        &mut self,
        name: &str,
        args: &[Expr],
        shapes: &[ShapeBind],
    ) -> Result<Val, String> {
        match name {
            "clog2" => {
                let v = self
                    .const_u64(&args[0])
                    .ok_or("prove: $clog2 with non-constant argument is not supported")?;
                let n = (64 - v.leading_zeros()).max(1) as u64;
                Ok(Val {
                    bits: (0..32)
                        .map(|i| self.enc.b.const_lit((n >> i) & 1 == 1))
                        .collect(),
                    signed: false,
                })
            }
            "unsigned" => {
                let mut v = self.eval(&args[0])?;
                v.signed = false;
                Ok(v)
            }
            "signed" => {
                let mut v = self.eval(&args[0])?;
                v.signed = true;
                Ok(v)
            }
            _ => self.call_user_fn(name, args, shapes),
        }
    }

    /// User function: inline by executing its body in slot space.
    fn call_user_fn(
        &mut self,
        name: &str,
        args: &[Expr],
        shapes: &[ShapeBind],
    ) -> Result<Val, String> {
        let fi = self
            .enc
            .flat
            .functions
            .iter()
            .position(|f| f.name == name)
            .ok_or_else(|| format!("prove: unknown function '{}'", name))?;
        let f = self.enc.flat.functions[fi].clone();
        let mut argv = Vec::new();
        for a in args {
            argv.push(self.eval(a)?.bits);
        }
        let locals = self.fn_locals(&f, argv, shapes)?;
        let saved_locals = self.locals.replace(locals);
        let saved_env = std::mem::take(&mut self.env);
        let saved_nba = std::mem::take(&mut self.nba_env);
        let r = self.exec_stmt(&f.body);
        let mut locals = self.locals.take().unwrap_or_default();
        self.locals = saved_locals;
        self.env = saved_env;
        self.nba_env = saved_nba;
        r?;
        let ret = locals
            .remove(&0)
            .unwrap_or_else(|| vec![self.enc.b.false_lit()]);
        Ok(Val {
            bits: resize_bits(&self.enc.b, &ret, f.ret_width.max(1)),
            signed: false,
        })
    }

    /// Build the callee slot space: argument slots, then hidden shape slots.
    fn fn_locals(
        &mut self,
        f: &FlatFunc,
        argv: Vec<Vec<Lit>>,
        shapes: &[ShapeBind],
    ) -> Result<HashMap<u32, Vec<Lit>>, String> {
        let mut locals: HashMap<u32, Vec<Lit>> = HashMap::new();
        for (i, av) in argv.iter().enumerate() {
            let w = f.slots.get(i).copied().unwrap_or(1).max(1);
            let bits = resize_bits(&self.enc.b, av, w);
            locals.insert(i as u32, bits);
        }
        // Hidden shape slots: the bound lo/asc evaluate in the caller's
        // context; an unbound formal takes its fallback convention (see
        // `Sim::fill_hidden`).
        for h in &f.hidden {
            let (lo, asc) = match shapes.iter().find(|b| b.formal == h.formal) {
                Some(b) => (self.eval(&b.lo)?.bits, self.eval(&b.asc)?.bits),
                None => {
                    let fb = |v: u64| {
                        (0..32)
                            .map(|i| self.enc.b.const_lit((v >> i) & 1 == 1))
                            .collect::<Vec<_>>()
                    };
                    (fb(h.fb_lo.max(0) as u64), fb(h.fb_asc as u64))
                }
            };
            locals.insert(h.lo, resize_bits(&self.enc.b, &lo, 32));
            locals.insert(h.asc, resize_bits(&self.enc.b, &asc, 32));
        }
        Ok(locals)
    }
}

/// Binary operator table. `l2`/`r2` are the context-extended operands; shifts
/// read the un-extended right operand (`rv`, self-determined amount).
fn eval_bin_op(
    b: &mut Builder,
    op: BinOp,
    l2: &[Lit],
    r2: &[Lit],
    rv: &[Lit],
    w: usize,
    signed: bool,
) -> Result<Vec<Lit>, String> {
    Ok(match op {
        BinOp::Add => add_bits(b, l2, r2, b.false_lit(), w).0,
        BinOp::Sub => sub_bits(b, l2, r2, w),
        BinOp::Mul => mul_bits(b, l2, r2, w),
        BinOp::Div => divmod_bits(b, l2, r2, w).0,
        BinOp::Mod => divmod_bits(b, l2, r2, w).1,
        BinOp::Shl => {
            let m = extend(b, rv, 6, false);
            shift_bits(b, l2, &m, w, ShiftDir::Left, None)
        }
        BinOp::Shr => {
            let m = extend(b, rv, 6, false);
            shift_bits(b, l2, &m, w, ShiftDir::Right, None)
        }
        BinOp::Sar => {
            let m = extend(b, rv, 6, false);
            let sign = if w > 0 { Some(l2[w - 1]) } else { None };
            shift_bits(b, l2, &m, w, ShiftDir::Right, sign)
        }
        BinOp::Rol | BinOp::Ror => {
            return Err("prove: rotate operators (rol/ror) unsupported".to_string());
        }
        BinOp::Pow => {
            return Err("prove: '**' operator unsupported".to_string());
        }
        _ => eval_bin_logic(b, op, l2, r2, w, signed),
    })
}

/// Comparison, bitwise, and logical operators (1-bit results for comparisons
/// and logical ops, vectors for bitwise ops).
fn eval_bin_logic(
    b: &mut Builder,
    op: BinOp,
    l2: &[Lit],
    r2: &[Lit],
    w: usize,
    signed: bool,
) -> Vec<Lit> {
    match op {
        BinOp::Lt => vec![lt_bits(b, l2, r2, w, signed)],
        BinOp::Le => {
            let lt = lt_bits(b, l2, r2, w, signed);
            let eq = eq_bits(b, l2, r2, w);
            vec![b.or_lit(lt, eq)]
        }
        BinOp::Gt => vec![lt_bits(b, r2, l2, w, signed)],
        BinOp::Ge => vec![ge_bits(b, l2, r2, w, signed)],
        BinOp::Eq | BinOp::EqStrict => vec![eq_bits(b, l2, r2, w)],
        BinOp::Ne | BinOp::NeStrict => vec![neg(eq_bits(b, l2, r2, w))],
        BinOp::BAnd => and_vec(b, l2, r2),
        BinOp::BOr => or_vec(b, l2, r2),
        BinOp::BXor => xor_vec(b, l2, r2),
        BinOp::BNand => not_bits(&and_vec(b, l2, r2)),
        BinOp::BNor => not_bits(&or_vec(b, l2, r2)),
        BinOp::BXnor => not_bits(&xor_vec(b, l2, r2)),
        BinOp::LogAnd => {
            let ta = or_bits(b, l2);
            let tb = or_bits(b, r2);
            vec![b.and_lit(ta, tb)]
        }
        BinOp::LogOr => {
            let ta = or_bits(b, l2);
            let tb = or_bits(b, r2);
            vec![b.or_lit(ta, tb)]
        }
        // Rejected by the guard in `eval_bin`.
        BinOp::EqM | BinOp::NeM | BinOp::LtM | BinOp::LeM | BinOp::GtM | BinOp::GeM => {
            vec![b.false_lit()]
        }
        // Handled by `eval_bin_op`.
        BinOp::Add
        | BinOp::Sub
        | BinOp::Mul
        | BinOp::Div
        | BinOp::Mod
        | BinOp::Shl
        | BinOp::Shr
        | BinOp::Sar
        | BinOp::Rol
        | BinOp::Ror
        | BinOp::Pow => vec![b.false_lit()],
    }
}
