//! Constant folding for BMC indices and loop bounds: pure `bool`-bit
//! arithmetic with no CNF gates. Split out of `bmc.rs`.

use super::exec::Exec;
use crate::sat::Lit;
use hdl_ir::elab::FlatSigId;
use hdl_ir::value::V1;
use hdl_ir::{BinOp, Expr, UnOp};

/// A compile-time constant value (used for indices / loop bounds).
struct CVal {
    bits: Vec<bool>,
    signed: bool,
}

impl<'e, 'a> Exec<'e, 'a> {
    pub(super) fn const_u64(&self, e: &Expr) -> Option<u64> {
        let v = self.const_val(e)?;
        let mut acc: u64 = 0;
        for (i, &b) in v.bits.iter().enumerate() {
            if i < 64 && b {
                acc |= 1 << i;
            }
        }
        Some(acc)
    }

    fn const_val(&self, e: &Expr) -> Option<CVal> {
        match e {
            Expr::Lit(v) => Some(CVal {
                bits: v.bits.iter().map(|&b| b == V1).collect(),
                signed: false,
            }),
            Expr::Sig(s) => self.const_sig_val(s),
            Expr::Bin { op, l, r } => {
                let lv = self.const_val(l)?;
                let rv = self.const_val(r)?;
                let w = lv.bits.len().max(rv.bits.len()).max(1);
                let signed = lv.signed || rv.signed;
                let l2 = const_extend(&lv.bits, w, signed);
                let r2 = const_extend(&rv.bits, w, signed);
                let bits = const_binop(*op, &l2, &r2, w, signed)?;
                Some(CVal { bits, signed })
            }
            Expr::Un { op, e } => {
                let v = self.const_val(e)?;
                Some(CVal {
                    bits: const_unop(*op, &v.bits)?,
                    signed: false,
                })
            }
            Expr::Cond { c, t, e } => {
                let cv = self.const_val(c)?;
                if const_truthy(&cv.bits) {
                    self.const_val(t)
                } else {
                    self.const_val(e)
                }
            }
            Expr::Resize { e, width, signed } => {
                let v = self.const_val(e)?;
                Some(CVal {
                    bits: const_extend(&v.bits, *width, *signed),
                    signed: *signed,
                })
            }
            Expr::Signed(e) => {
                let mut v = self.const_val(e)?;
                v.signed = true;
                Some(v)
            }
            _ => None,
        }
    }

    /// A signal is constant when its current env/locals bits are all constant
    /// literals.
    fn const_sig_val(&self, s: &FlatSigId) -> Option<CVal> {
        let lits = if let Some(l) = &self.locals {
            l.get(s).cloned()
        } else {
            self.env.get(s).cloned()
        }?;
        let bits: Vec<bool> = lits
            .iter()
            .map(|&l| {
                if l == self.enc.b.true_lit() {
                    Some(true)
                } else if l == self.enc.b.false_lit() {
                    Some(false)
                } else {
                    None
                }
            })
            .collect::<Option<Vec<bool>>>()?;
        Some(CVal {
            bits,
            signed: self.enc.flat.signals[*s as usize].signed,
        })
    }
}

/// If all bits are constant literals, return them as bools.
pub(super) fn const_bits(bits: &[Lit], ex: &Exec) -> Option<Vec<bool>> {
    bits.iter()
        .map(|&l| {
            if l == ex.enc.b.true_lit() {
                Some(true)
            } else if l == ex.enc.b.false_lit() {
                Some(false)
            } else {
                None
            }
        })
        .collect()
}

pub(super) fn const_truthy(bits: &[bool]) -> bool {
    bits.iter().any(|&b| b)
}

fn const_extend(bits: &[bool], w: usize, signed: bool) -> Vec<bool> {
    let mut out = bits.to_vec();
    if out.len() > w {
        out.truncate(w);
    } else {
        let fill = signed && out.last().copied().unwrap_or(false);
        while out.len() < w {
            out.push(fill);
        }
    }
    out
}

fn mask(w: usize) -> u64 {
    if w >= 64 {
        u64::MAX
    } else {
        (1u64 << w) - 1
    }
}

fn const_to_u(v: &[bool]) -> u64 {
    let mut x = 0u64;
    for (i, &bit) in v.iter().enumerate() {
        if i < 64 && bit {
            x |= 1 << i;
        }
    }
    x
}

fn const_to_i(v: &[bool], w: usize, signed: bool) -> i128 {
    let mut x: i128 = 0;
    for (i, &bit) in v.iter().enumerate() {
        if bit {
            x |= 1 << i;
        }
    }
    if signed && w < 128 && v.get(w - 1).copied().unwrap_or(false) {
        x |= -1i128 << w;
    }
    x
}

fn const_bits_of(v: u64, w: usize) -> Vec<bool> {
    (0..w).map(|i| (v >> i) & 1 == 1).collect()
}

/// Arithmetic/shift operators produce a w-bit result; comparisons, bitwise
/// and logical operators fall through to [`const_binop_rest`].
fn const_binop(op: BinOp, a: &[bool], b: &[bool], w: usize, signed: bool) -> Option<Vec<bool>> {
    let r = match op {
        BinOp::Add => const_to_u(a).wrapping_add(const_to_u(b)),
        BinOp::Sub => const_to_u(a).wrapping_sub(const_to_u(b)),
        BinOp::Mul => const_to_u(a).wrapping_mul(const_to_u(b)),
        BinOp::Div => const_to_u(a).checked_div(const_to_u(b)).unwrap_or(0),
        BinOp::Mod => {
            let (x, y) = (const_to_u(a), const_to_u(b));
            if y == 0 {
                0
            } else {
                x % y
            }
        }
        BinOp::Shl => const_to_u(a).wrapping_shl(const_to_u(b) as u32),
        BinOp::Shr => const_to_u(a).wrapping_shr(const_to_u(b) as u32),
        BinOp::Sar => {
            let sh = (const_to_u(b) as u32) % 64;
            if signed {
                ((const_to_i(a, w, signed) >> sh) as u64) & mask(w)
            } else {
                const_to_u(a) >> sh
            }
        }
        _ => return const_binop_rest(op, a, b, w, signed),
    };
    Some(const_bits_of(r, w))
}

/// Comparisons yield a one-bit vector; bitwise ops are per-bit; logical ops
/// test the whole vector.
fn const_binop_rest(
    op: BinOp,
    a: &[bool],
    b: &[bool],
    w: usize,
    signed: bool,
) -> Option<Vec<bool>> {
    Some(match op {
        BinOp::Lt => vec![if signed {
            const_to_i(a, w, signed) < const_to_i(b, w, signed)
        } else {
            const_to_u(a) < const_to_u(b)
        }],
        BinOp::Le => vec![if signed {
            const_to_i(a, w, signed) <= const_to_i(b, w, signed)
        } else {
            const_to_u(a) <= const_to_u(b)
        }],
        BinOp::Gt => vec![if signed {
            const_to_i(a, w, signed) > const_to_i(b, w, signed)
        } else {
            const_to_u(a) > const_to_u(b)
        }],
        BinOp::Ge => vec![if signed {
            const_to_i(a, w, signed) >= const_to_i(b, w, signed)
        } else {
            const_to_u(a) >= const_to_u(b)
        }],
        BinOp::Eq | BinOp::EqStrict => vec![a == b],
        BinOp::Ne | BinOp::NeStrict => vec![a != b],
        BinOp::BAnd => (0..w).map(|i| a[i] && b[i]).collect(),
        BinOp::BOr => (0..w).map(|i| a[i] || b[i]).collect(),
        BinOp::BXor => (0..w).map(|i| a[i] != b[i]).collect(),
        BinOp::BNand => (0..w).map(|i| !(a[i] && b[i])).collect(),
        BinOp::BNor => (0..w).map(|i| !(a[i] || b[i])).collect(),
        BinOp::BXnor => (0..w).map(|i| a[i] == b[i]).collect(),
        BinOp::LogAnd => vec![const_to_u(a) != 0 && const_to_u(b) != 0],
        BinOp::LogOr => vec![const_to_u(a) != 0 || const_to_u(b) != 0],
        _ => return None,
    })
}

fn const_unop(op: UnOp, a: &[bool]) -> Option<Vec<bool>> {
    let w = a.len().max(1);
    Some(match op {
        UnOp::Not => vec![!a.iter().any(|&b| b)],
        UnOp::BNot => a.iter().map(|&b| !b).collect(),
        UnOp::Neg => {
            let x = const_to_u(a);
            (0..w).map(|i| (x.wrapping_neg() >> i) & 1 == 1).collect()
        }
        UnOp::Pos => a.to_vec(),
        UnOp::Abs => {
            // two's-complement absolute value
            let sign = a.last().copied().unwrap_or(false);
            if sign {
                let x = const_to_u(a);
                (0..a.len())
                    .map(|i| (x.wrapping_neg() >> i) & 1 == 1)
                    .collect()
            } else {
                a.to_vec()
            }
        }
        UnOp::RedAnd => vec![a.iter().all(|&b| b)],
        UnOp::RedOr => vec![a.iter().any(|&b| b)],
        UnOp::RedXor => vec![a.iter().fold(false, |acc, &b| acc != b)],
        UnOp::RedNand => vec![!a.iter().all(|&b| b)],
        UnOp::RedNor => vec![!a.iter().any(|&b| b)],
        UnOp::RedXnor => vec![!a.iter().fold(false, |acc, &b| acc != b)],
    })
}
