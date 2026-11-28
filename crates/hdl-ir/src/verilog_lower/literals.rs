//! Verilog literal-text decoding: plain/based integers, reals, and SV time
//! literals (`1ns`) → [`Value`]/f64/fs. Child module of `verilog_lower`; the
//! lowerer's `lower_literal` and the kernel's based-literal reader both go
//! through [`parse_int_literal`].

use crate::value::{Value, V0, V1, VX, VZ};

/// Parse a Verilog integer literal: `[size]'[s]?base digits` or plain
/// decimal. Unsized literals are 32 bits; sized keep their declared width.
pub fn parse_int_literal(t: &str) -> Option<Value> {
    let t = t.trim();
    let (size, rest) = match t.find('\'') {
        Some(q) => {
            let (sz, r) = t.split_at(q);
            let size = if sz.is_empty() {
                None
            } else {
                Some(sz.parse::<u64>().ok()?)
            };
            (size, &r[1..])
        }
        None => (None, t),
    };
    let rest = rest.trim_start_matches(['s', 'S']);
    let (base, digits) = match rest.chars().next()? {
        'b' | 'B' => (2u32, &rest[1..]),
        'o' | 'O' => (8, &rest[1..]),
        'd' | 'D' => (10, &rest[1..]),
        'h' | 'H' => (16, &rest[1..]),
        c if c.is_ascii_digit() => (10, rest),
        _ => return None,
    };
    let digits = digits.replace('_', "");
    if digits.is_empty() {
        return None;
    }
    // Verilog unsized literals (plain decimal or `'b...` without a size) are
    // at least 32 bits wide; sized literals keep their declared size.
    let width = size.map(|s| s as usize).unwrap_or(32);
    if !digits
        .chars()
        .any(|c| matches!(c.to_ascii_lowercase(), 'x' | 'z' | '?'))
    {
        known_bits(&digits, base, width)
    } else {
        Some(xz_bits(&digits, base, width))
    }
}

/// All-known digits → a `width`-wide value (None on overflow).
fn known_bits(digits: &str, base: u32, width: usize) -> Option<Value> {
    let v = u128::from_str_radix(digits, base).ok()?;
    let mut bits = vec![V0; width.max(1)];
    for (i, b) in bits.iter_mut().enumerate().take(width.min(128)) {
        *b = if (v >> i) & 1 == 1 { V1 } else { V0 };
    }
    Some(Value { bits, real: false })
}

/// Digits containing `x`/`z`/`?`: each digit expands to its `digit_bits`
/// value bits, `x`/`z` filling their digit's slots with X/Z.
fn xz_bits(digits: &str, base: u32, width: usize) -> Value {
    let digit_bits = match base {
        2 => 1,
        8 => 3,
        16 => 4,
        _ => 1,
    };
    let mut bits = vec![VX; width.max(1)];
    for (i, c) in digits.chars().enumerate() {
        let from_lsb = (digits.len() - 1 - i) * digit_bits;
        let fill = match c.to_ascii_lowercase() {
            'x' | '?' => VX,
            'z' => VZ,
            _ => {
                if let Some(d) = c.to_digit(base) {
                    for j in 0..digit_bits {
                        let idx = from_lsb + j;
                        if idx < bits.len() {
                            bits[idx] = if (d >> j) & 1 == 1 { V1 } else { V0 };
                        }
                    }
                }
                continue;
            }
        };
        for j in 0..digit_bits {
            let idx = from_lsb + j;
            if idx < bits.len() {
                bits[idx] = fill;
            }
        }
    }
    Value { bits, real: false }
}

/// Parse a Verilog real literal (`1.5`, `1e-3`, digit separators allowed).
pub(crate) fn parse_real_literal(t: &str) -> Option<f64> {
    let t = t.trim();
    if t.contains('\'') {
        return None;
    }
    if !(t.contains('.') || t.contains('e') || t.contains('E')) {
        return None;
    }
    let c: String = t.chars().filter(|&c| c != '_').collect();
    c.parse::<f64>().ok()
}

/// Parse an SV time literal (`1ns`, `1.5us`, `1 ps`) into absolute
/// femtoseconds (SV LRM 5.7.1: number + time unit `fs|ps|ns|us|ms|s`).
/// `None` when the text is not a number followed by a known unit.
pub(crate) fn time_literal_fs(t: &str) -> Option<u64> {
    let t = t.trim();
    let cut = t.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '_'))?;
    let (num, unit) = t.split_at(cut);
    let mult: f64 = match unit.trim() {
        "fs" => 1.0,
        "ps" => 1e3,
        "ns" => 1e6,
        "us" => 1e9,
        "ms" => 1e12,
        "s" => 1e15,
        _ => return None,
    };
    let v = num.replace('_', "").parse::<f64>().ok()?;
    Some((v * mult) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_int_literals() {
        assert_eq!(parse_int_literal("42").unwrap().to_u64(), Some(42));
        assert_eq!(parse_int_literal("8'hFF").unwrap().to_u64(), Some(255));
        assert_eq!(parse_int_literal("'b1010").unwrap().to_u64(), Some(10));
        assert_eq!(parse_int_literal("4'd3").unwrap().to_u64(), Some(3));
        let x = parse_int_literal("4'b10x1").unwrap();
        assert_eq!(x.to_u64(), None);
        assert_eq!(x.bit(0), V1);
        assert_eq!(x.bit(1), VX);
        assert_eq!(parse_int_literal("4'b1z1").unwrap().bit(1), VZ);
        assert_eq!(parse_int_literal("16'hFF").unwrap().width(), 16);
        assert_eq!(parse_int_literal("1_000").unwrap().to_u64(), Some(1000));
        assert_eq!(parse_int_literal("8'shFF").unwrap().width(), 8);
        assert_eq!(parse_int_literal(""), None);
        assert_eq!(parse_int_literal("8'h"), None);
    }

    #[test]
    fn parse_real_literals() {
        assert_eq!(parse_real_literal("1.5"), Some(1.5));
        assert_eq!(parse_real_literal("2.0E-2"), Some(0.02));
        assert_eq!(parse_real_literal("1e3"), Some(1000.0));
        assert_eq!(parse_real_literal(".5"), Some(0.5));
        assert_eq!(parse_real_literal("42"), None); // plain integer
        assert_eq!(parse_real_literal("8'hFF"), None); // based literal
        assert_eq!(parse_real_literal("8'h1E"), None); // 'E' inside based literal
        assert_eq!(parse_real_literal("1_000.5"), Some(1000.5));
    }

    #[test]
    fn parse_time_literals() {
        assert_eq!(time_literal_fs("1ns"), Some(1_000_000));
        assert_eq!(time_literal_fs("1 ns"), Some(1_000_000)); // spaced form
        assert_eq!(time_literal_fs("1.5us"), Some(1_500_000_000));
        assert_eq!(time_literal_fs("2ps"), Some(2_000));
        assert_eq!(time_literal_fs("1_0ns"), Some(10_000_000)); // digit separators
        assert_eq!(time_literal_fs("42"), None); // plain number has no unit
        assert_eq!(time_literal_fs("1e3"), None); // exponent real, not a unit
        assert_eq!(time_literal_fs("1q"), None); // not a time unit
    }
}
