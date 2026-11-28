//! Bit-vector helpers over CNF literals (LSB-first), all Tseitin-encoded.
//! Pure move from `bmc.rs`.

use super::builder::Builder;
use crate::sat::{neg, Lit};

pub(super) fn or_bits(b: &mut Builder, bits: &[Lit]) -> Lit {
    let mut acc = b.false_lit();
    for &x in bits {
        acc = b.or_lit(acc, x);
    }
    acc
}

pub(super) fn and_bits(b: &mut Builder, bits: &[Lit]) -> Lit {
    let mut acc = b.true_lit();
    for &x in bits {
        acc = b.and_lit(acc, x);
    }
    acc
}

pub(super) fn not_bits(bits: &[Lit]) -> Vec<Lit> {
    bits.iter().map(|&l| neg(l)).collect()
}

pub(super) fn and_vec(b: &mut Builder, a: &[Lit], c: &[Lit]) -> Vec<Lit> {
    a.iter()
        .zip(c.iter())
        .map(|(&x, &y)| b.and_lit(x, y))
        .collect()
}

pub(super) fn or_vec(b: &mut Builder, a: &[Lit], c: &[Lit]) -> Vec<Lit> {
    a.iter()
        .zip(c.iter())
        .map(|(&x, &y)| b.or_lit(x, y))
        .collect()
}

pub(super) fn xor_vec(b: &mut Builder, a: &[Lit], c: &[Lit]) -> Vec<Lit> {
    a.iter()
        .zip(c.iter())
        .map(|(&x, &y)| b.xor_lit(x, y))
        .collect()
}

pub(super) fn mux_vec(b: &mut Builder, c: Lit, a: &[Lit], d: &[Lit]) -> Vec<Lit> {
    a.iter()
        .zip(d.iter())
        .map(|(&x, &y)| b.mux_lit(c, x, y))
        .collect()
}

/// Zero- or sign-extend (or truncate) a bit vector to width `w`.
pub(super) fn extend(b: &Builder, bits: &[Lit], w: usize, signed: bool) -> Vec<Lit> {
    let mut out = bits.to_vec();
    if out.len() > w {
        out.truncate(w);
    } else {
        let fill = if signed && !out.is_empty() {
            *out.last().expect("checked !out.is_empty()")
        } else {
            b.false_lit()
        };
        while out.len() < w {
            out.push(fill);
        }
    }
    out
}

pub(super) fn resize_bits(b: &Builder, bits: &[Lit], w: usize) -> Vec<Lit> {
    extend(b, bits, w, false)
}

/// Ripple-carry add: returns (sum bits, carry out).
pub(super) fn add_bits(
    b: &mut Builder,
    a: &[Lit],
    c: &[Lit],
    carry: Lit,
    w: usize,
) -> (Vec<Lit>, Lit) {
    let mut sum = Vec::with_capacity(w);
    let mut carry = carry;
    for i in 0..w {
        let x = a[i];
        let y = c[i];
        let axb = b.xor_lit(x, y);
        sum.push(b.xor_lit(axb, carry));
        // carry = (x & y) | (carry & (x ^ y))
        let c1 = b.and_lit(x, y);
        let c2 = b.and_lit(carry, axb);
        carry = b.or_lit(c1, c2);
    }
    (sum, carry)
}

pub(super) fn sub_bits(b: &mut Builder, a: &[Lit], c: &[Lit], w: usize) -> Vec<Lit> {
    add_bits(b, a, &not_bits(c), b.true_lit(), w).0
}

/// Shift-add multiply, result truncated to w bits.
pub(super) fn mul_bits(b: &mut Builder, a: &[Lit], c: &[Lit], w: usize) -> Vec<Lit> {
    let mut acc = vec![b.false_lit(); w];
    for i in 0..w {
        // shifted[j] = (j >= i) ? a[j - i] : 0, gated by c[i]
        let partial: Vec<Lit> = (0..w)
            .map(|j| {
                let s = if j >= i { a[j - i] } else { b.false_lit() };
                b.and_lit(s, c[i])
            })
            .collect();
        acc = add_bits(b, &acc, &partial, b.false_lit(), w).0;
    }
    acc
}

/// Restoring division: returns (quotient, remainder); divisor 0 yields zeros
/// (the simulator produces X there; 2-valued BMC picks 0 — documented).
pub(super) fn divmod_bits(b: &mut Builder, a: &[Lit], c: &[Lit], w: usize) -> (Vec<Lit>, Vec<Lit>) {
    let wc = extend(b, c, w + 1, false);
    let mut rem = vec![b.false_lit(); w + 1];
    let mut quot = vec![b.false_lit(); w];
    for i in (0..w).rev() {
        // rem = (rem << 1) | a[i]
        let mut nrem = vec![a[i]];
        nrem.extend_from_slice(&rem[..w]);
        let ge = ge_bits(b, &nrem, &wc, w + 1, false);
        let diff = add_bits(b, &nrem, &not_bits(&wc), b.true_lit(), w + 1).0;
        rem = mux_vec(b, ge, &diff, &nrem);
        quot[i] = ge;
    }
    (quot, resize_bits(b, &rem, w))
}

/// Less-than over w-bit vectors (unsigned, or signed when `signed`).
pub(super) fn lt_bits(b: &mut Builder, a: &[Lit], c: &[Lit], w: usize, signed: bool) -> Lit {
    let mut lt = b.false_lit();
    let mut eq = b.true_lit();
    for i in (0..w).rev() {
        let t1 = b.and_lit(eq, neg(a[i]));
        let t2 = b.and_lit(t1, c[i]);
        lt = b.or_lit(lt, t2);
        let e = b.eq_lit(a[i], c[i]);
        eq = b.and_lit(eq, e);
    }
    if signed && w > 0 {
        let sa = a[w - 1];
        let sc = c[w - 1];
        // a <s c  iff  (¬sa ∧ sc) ∨ (sa == sc ∧ a <u c)
        let d1 = b.and_lit(neg(sa), sc);
        let s_eq = b.eq_lit(sa, sc);
        let d2 = b.and_lit(s_eq, lt);
        b.or_lit(d1, d2)
    } else {
        lt
    }
}

/// Greater-or-equal over w-bit vectors.
pub(super) fn ge_bits(b: &mut Builder, a: &[Lit], c: &[Lit], w: usize, signed: bool) -> Lit {
    let lt = lt_bits(b, c, a, w, signed);
    let eq = eq_bits(b, a, c, w);
    b.or_lit(lt, eq)
}

/// Equality over w bits (vectors pre-extended to w).
pub(super) fn eq_bits(b: &mut Builder, a: &[Lit], c: &[Lit], w: usize) -> Lit {
    let mut acc = b.true_lit();
    for i in 0..w {
        let e = b.eq_lit(a[i], c[i]);
        acc = b.and_lit(acc, e);
    }
    acc
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ShiftDir {
    Left,
    Right,
}

/// Barrel shift by the low 6 bits of the amount (mirrors u64 wrapping shifts).
pub(super) fn shift_bits(
    b: &mut Builder,
    a: &[Lit],
    m: &[Lit],
    w: usize,
    dir: ShiftDir,
    sign: Option<Lit>,
) -> Vec<Lit> {
    let mut cur = a.to_vec();
    for (k, &mb) in m.iter().enumerate() {
        if k >= 6 {
            break;
        }
        let s = 1usize << k;
        let shifted = shift_once(b, &cur, s, w, dir, sign);
        cur = mux_vec(b, mb, &shifted, &cur);
    }
    cur
}

fn shift_once(
    b: &Builder,
    a: &[Lit],
    s: usize,
    w: usize,
    dir: ShiftDir,
    sign: Option<Lit>,
) -> Vec<Lit> {
    let mut out = vec![b.false_lit(); w];
    match dir {
        ShiftDir::Left => {
            for j in 0..w {
                if j >= s {
                    out[j] = a[j - s];
                }
            }
        }
        ShiftDir::Right => {
            let fill = sign.unwrap_or_else(|| b.false_lit());
            for j in 0..w {
                if j + s < w {
                    out[j] = a[j + s];
                } else {
                    out[j] = fill;
                }
            }
        }
    }
    out
}
