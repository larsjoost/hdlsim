//! Value ↔ text and bit-level helpers shared by the kernel and its
//! submodules: string packing, VHDL radix images, sign extension, and the
//! `truthy`/`eq_strict`/`lhs_sig` predicates. Child module of `sim` so it
//! can use the kernel's imports; re-exported through `use value_util::*`.

use super::*;

/// Pack a string as a Value: first char in the high byte, MSB-first — the same
/// layout as VHDL/Verilog string literals.
pub(super) fn pack_str_value(s: &str) -> Value {
    let bytes = s.as_bytes();
    let n = bytes.len().max(1);
    let mut bits = vec![V0; n * 8];
    for (bi, &b) in bytes.iter().enumerate() {
        for j in 0..8 {
            bits[(n - 1 - bi) * 8 + j] = if (b >> j) & 1 == 1 { V1 } else { V0 };
        }
    }
    Value { bits, real: false }
}

/// Inverse of [`pack_str_value`]: bytes of a packed string Value → String.
pub(super) fn unpack_str_value(v: &Value) -> String {
    let n = v.bits.len() / 8;
    let mut s = String::new();
    for bi in 0..n {
        let mut b = 0u8;
        for j in 0..8 {
            if v.bits.get((n - 1 - bi) * 8 + j) == Some(&V1) {
                b |= 1 << j;
            }
        }
        s.push(b as char);
    }
    s
}

/// VHDL `to_hstring`/`to_ostring`/`to_bstring` of a value (MSB-first digits).
pub(super) fn vhdl_radix_string(v: &Value, radix: u32) -> String {
    let bits_per = match radix {
        16 => 4,
        8 => 3,
        _ => 1,
    };
    let w = v.bits.len();
    let pad = (bits_per - (w % bits_per)) % bits_per;
    let mut out = String::new();
    let mut i = w + pad;
    while i > 0 {
        i -= bits_per;
        let mut digit = 0u32;
        let mut known = true;
        for j in 0..bits_per {
            let idx = i + j;
            let b = if idx < w { v.bits[idx] } else { V0 };
            if b == V1 {
                digit |= 1 << j;
            } else if b != V0 {
                known = false;
            }
        }
        if radix == 2 {
            out.push(if known {
                if digit == 1 {
                    '1'
                } else {
                    '0'
                }
            } else {
                'X'
            });
        } else if known {
            out.push(
                char::from_digit(digit, radix)
                    .expect("digit < radix, so from_digit succeeds")
                    .to_ascii_uppercase(),
            );
        } else {
            out.push('X');
        }
    }
    out
}

/// Sign-extend `v` to `width` bits (VHDL signed semantics).
pub(super) fn sign_extend(v: &Value, width: usize) -> Value {
    let w = width.max(v.width()).max(1);
    let mut bits = vec![V0; w];
    bits[..v.width()].copy_from_slice(&v.bits);
    let sign = v.bit(v.width().saturating_sub(1));
    if sign != V0 && sign != V1 {
        bits[v.width()..w].fill(sign); // X/Z sign-extends as X/Z
    } else if sign == V1 {
        bits[v.width()..w].fill(V1);
    }
    Value { bits, real: false }
}

pub(super) fn truthy(v: &Value) -> bool {
    v.to_u64().is_some_and(|x| x != 0)
}

pub(super) fn eq_strict(a: &Value, b: &Value) -> bool {
    a.binop(BinOp::EqStrict, b).to_u64() == Some(1)
}

pub(super) fn lhs_sig(e: &Expr) -> Option<FlatSigId> {
    match e {
        Expr::Sig(s) => Some(*s),
        _ => None,
    }
}

pub(super) fn string_of(v: &Value) -> String {
    // Interpret value bits as bytes, MSB chunk first (first char = high byte,
    // matching how string literals are packed and Value::concat orders parts).
    let mut bytes = Vec::new();
    for chunk in v.bits.chunks(8).rev() {
        let mut byte = 0u8;
        for (j, &b) in chunk.iter().enumerate() {
            if b == V1 {
                byte |= 1 << j;
            }
        }
        if byte != 0 {
            bytes.push(byte);
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}
