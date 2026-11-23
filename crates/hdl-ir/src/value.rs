//! Multi-state logic values — the shared data model of the simulator.
//!
//! Each bit is one of the nine VHDL `std_logic` states; Verilog uses the 4-state
//! subset (`0`, `1`, `X`, `Z`). Bits are stored LSB-first: `bits[0]` is bit 0.
//!
//! Invariants:
//! - `bits[i]` is always one of the `V*` constants below.
//! - Arithmetic/bitwise results propagate X/Z: if any operand bit is not 0/1,
//!   the corresponding result bit is X (except `===`/`!==`, which compare X/Z).

use serde::{Serialize, Serializer};

/// Bit state constants (matching VHDL std_logic ordering where applicable).
pub const V0: u8 = 0;
pub const V1: u8 = 1;
pub const VX: u8 = 2;
pub const VZ: u8 = 3;
pub const VW: u8 = 4;
pub const VL: u8 = 5;
pub const VH: u8 = 6;
pub const VDASH: u8 = 7; // '-' (don't care)
pub const VU: u8 = 8; // 'U' (uninitialized)

pub fn is_known(b: u8) -> bool {
    b == V0 || b == V1
}

/// Is this bit X or Z (unknown / high-impedance)?
pub fn is_xz(b: u8) -> bool {
    matches!(b, VX | VZ | VW | VU)
}

/// Loose equality between a narrow bit vector and a wide byte-packed string of
/// '0'/'1' characters (chars MSB-first, first char in the high byte — the
/// string-literal packing): true when the wide value is exactly 8x the narrow
/// one and each byte is the char of the corresponding bit.
fn eq_string_loose(a: &Value, b: &Value) -> bool {
    let (narrow, wide) = if a.width() <= b.width() {
        (a, b)
    } else {
        (b, a)
    };
    let nb = narrow.width();
    if nb == 0 || wide.width() != nb * 8 || !wide.is_known() || !narrow.is_known() {
        return false;
    }
    let nbytes = wide.width() / 8;
    (0..nb).all(|i| {
        // String chars are MSB-first (char 0 = high byte), so bit i is char
        // `nb - 1 - i`, which sits in byte `nbytes - 1 - (nb - 1 - i)`.
        let byte_idx = nbytes - nb + i;
        let byte_bits = &wide.bits[byte_idx * 8..byte_idx * 8 + 8];
        let byte = byte_bits
            .iter()
            .enumerate()
            .fold(0u8, |acc, (j, &b)| acc | (((b == V1) as u8) << j));
        byte == if narrow.bits[i] == V1 { b'1' } else { b'0' }
    })
}

#[derive(Clone, Debug)]
pub struct Value {
    pub bits: Vec<u8>,
    /// Real-typed marker: the bits are an IEEE-754 f64 (VHDL `real`).
    /// Widths alone cannot tell a 64-bit vector from a real; function
    /// frames hold real formals/locals that only this marker identifies.
    /// Equality ignores it (same bits compare equal).
    pub real: bool,
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        self.bits == other.bits
    }
}

impl Eq for Value {}

/// JSON output mirrors the VCD sink's two formats: an all-0/1 value is
/// `{"w": <bits>, "hex": "<value>"}` (4 bits per digit, MSB first, like every
/// hex tool), anything with x/z/metavalues is `{"w": <bits>, "bits": "<states>"}`
/// (one lowercase state letter per bit, MSB first, weak values included —
/// they must not alias to strong 0/1). Signal initializers dominate an IR
/// dump; per-bit JSON number arrays would be ~8× larger.
impl Serialize for Value {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        const D: &[u8; 16] = b"0123456789abcdef";
        const L: &[u8; 9] = b"01xzwlh-u";
        let mut st = s.serialize_struct("Value", 2)?;
        st.serialize_field("w", &self.bits.len())?;
        if self.bits.iter().all(|&b| b == V0 || b == V1) {
            let mut hex = String::with_capacity(self.bits.len().div_ceil(4));
            let (mut nib, mut n) = (0u8, 0u32);
            for &b in self.bits.iter().rev() {
                nib = (nib << 1) | u8::from(b == V1);
                n += 1;
                if n == 4 {
                    hex.push(D[nib as usize] as char);
                    (nib, n) = (0, 0);
                }
            }
            if n > 0 {
                hex.push(D[nib as usize] as char);
            }
            st.serialize_field("hex", &hex)?;
        } else {
            let bits: String = self
                .bits
                .iter()
                .rev()
                .map(|&b| L[b as usize] as char)
                .collect();
            st.serialize_field("bits", &bits)?;
        }
        st.end()
    }
}

impl Value {
    pub fn new(width: usize, fill: u8) -> Value {
        Value {
            bits: vec![fill; width.max(1)],
            real: false,
        }
    }

    pub fn from_u64(width: usize, v: u64) -> Value {
        let mut bits = vec![V0; width.max(1)];
        for i in 0..width.min(64) {
            bits[i] = if (v >> i) & 1 == 1 { V1 } else { V0 };
        }
        Value { bits, real: false }
    }

    /// All-X value of the given width.
    pub fn x(width: usize) -> Value {
        Value::new(width, VX)
    }

    /// Single-bit value from a bit state constant (`V0`, `V1`, `VX`, …).
    pub fn from_bit(b: u8) -> Value {
        Value {
            bits: vec![b],
            real: false,
        }
    }

    /// 64-bit value holding the IEEE-754 bit pattern of `x` (a `real` signal).
    pub fn from_f64(x: f64) -> Value {
        Value {
            bits: Value::from_u64(64, x.to_bits()).bits,
            real: true,
        }
    }

    /// Interpret a 64-bit value as f64 (real signals). None if not 64 known bits.
    pub fn to_f64(&self) -> Option<f64> {
        if self.bits.len() != 64 || !self.is_known() {
            return None;
        }
        Some(f64::from_bits(self.to_u64()?))
    }

    pub fn width(&self) -> usize {
        self.bits.len()
    }

    pub fn bit(&self, i: usize) -> u8 {
        if i < self.bits.len() {
            self.bits[i]
        } else {
            VX // out-of-range read is X in Verilog
        }
    }

    pub fn set_bit(&mut self, i: usize, b: u8) {
        if i < self.bits.len() {
            self.bits[i] = b;
        }
    }

    /// Numeric value if all bits are 0/1, else None.
    pub fn to_u64(&self) -> Option<u64> {
        let mut v = 0u64;
        for (i, &b) in self.bits.iter().enumerate() {
            if !is_known(b) {
                return None;
            }
            if i < 64 && b == V1 {
                v |= 1 << i;
            }
        }
        Some(v)
    }

    /// Numeric value sign-extended from the value's own width: a width-32
    /// `integer` holding -1 (`0xFFFF_FFFF`) is -1, not 4294967295. Range
    /// bounds and other signed-integer contexts need this — `to_u64`
    /// turns a negative bound into a near-2⁶⁴ count and range arithmetic
    /// underflows into runaway widths/depths.
    pub fn to_i64(&self) -> Option<i64> {
        let u = self.to_u64()?;
        let w = self.width().min(64) as u32;
        if w > 0 && w < 64 && (u >> (w - 1)) & 1 == 1 {
            Some(u as i64 - (1i64 << w))
        } else {
            Some(u as i64)
        }
    }

    pub fn is_known(&self) -> bool {
        self.bits.iter().all(|&b| is_known(b))
    }

    /// Whether any bit is a metavalue (x/z/w/…). X-only debug filters use
    /// this to catch transitions into unknown states.
    pub fn has_meta(&self) -> bool {
        self.bits.iter().any(|&b| !is_known(b))
    }

    /// Compact single-token rendering for logs and watch lines. Real-typed
    /// values decode the f64; all-0/1 values print as `0b…` (≤8 bits) or
    /// `0x…`; anything with metavarues prints one state letter per bit,
    /// MSB first (same alphabet as the JSON serialization, quoted so a
    /// leading `-` or letter cannot be read as a number).
    pub fn fmt_value(&self) -> String {
        if self.real {
            if let Some(u) = self.to_u64() {
                return format!("r={}", f64::from_bits(u));
            }
        }
        if !self.has_meta() {
            let w = self.bits.len();
            if w <= 8 {
                let mut s = String::with_capacity(2 + w);
                s.push_str("0b");
                for &b in self.bits.iter().rev() {
                    s.push(if b == V1 { '1' } else { '0' });
                }
                return s;
            }
            let mut hex = String::with_capacity(2 + w.div_ceil(4));
            hex.push_str("0x");
            let (mut nib, mut n, mut used) = (0u8, 0u32, 0usize);
            for &b in self.bits.iter().rev() {
                nib = (nib << 1) | u8::from(b == V1);
                n += 1;
                used += 1;
                if n == 4 || used == w {
                    // Suppress leading zero nibbles on the first digit.
                    if !(hex.len() == 2 && nib == 0 && used < w) {
                        hex.push(char::from_digit(nib as u32, 16).unwrap_or('0'));
                    }
                    (nib, n) = (0, 0);
                }
            }
            return hex;
        }
        const L: &[u8; 9] = b"01xzwlh-u";
        let mut s = String::with_capacity(1 + self.bits.len());
        s.push('"');
        for &b in self.bits.iter().rev() {
            s.push(L[b as usize] as char);
        }
        s.push('"');
        s
    }

    /// Resize to `width` bits (zero-extend; Verilog unsigned default).
    pub fn resize(&self, width: usize) -> Value {
        let mut bits = vec![V0; width.max(1)];
        for i in 0..width.min(self.bits.len()) {
            bits[i] = self.bits[i];
        }
        // Extension bits of an X/Z value stay X.
        if self.bits.iter().any(|&b| !is_known(b)) && width > self.bits.len() {
            for i in self.bits.len()..width {
                bits[i] = VX;
            }
        }
        Value {
            bits,
            real: self.real,
        }
    }

    /// VHDL-2008 matching equality (`?=` / `case?`): choice bits of `VDASH`
    /// ('-') are wildcards; every other bit must be equal. Both sides are
    /// extended to the wider width first.
    pub fn matches_case(&self, choice: &Value) -> bool {
        let w = self.width().max(choice.width()).max(1);
        let a = self.resize(w);
        let b = choice.resize(w);
        a.bits
            .iter()
            .zip(b.bits.iter())
            .all(|(x, c)| *c == VDASH || x == c)
    }

    /// Concatenate: `parts[0]` is the most significant part (Verilog `{a, b}`).
    pub fn concat(parts: &[Value]) -> Value {
        let total: usize = parts.iter().map(|p| p.width()).sum();
        let mut bits = vec![VX; total.max(1)];
        let mut shift = 0;
        for p in parts.iter().rev() {
            for (i, &b) in p.bits.iter().enumerate() {
                bits[shift + i] = b;
            }
            shift += p.width();
        }
        Value { bits, real: false }
    }

    /// Slice `bits[lsb..=msb]` (inclusive, Verilog `[msb:lsb]`).
    pub fn slice(&self, msb: usize, lsb: usize) -> Value {
        let msb = msb.min(self.bits.len().saturating_sub(1));
        let lsb = lsb.min(msb);
        Value {
            bits: self.bits[lsb..=msb].to_vec(),
            real: false,
        }
    }

    /// Bitwise/logical op with X/Z propagation. `strict` ops (===, !==) compare
    /// X/Z literally and never produce X.
    pub fn binop(&self, op: BinOp, rhs: &Value) -> Value {
        use BinOp::*;
        // Shift/rotate results have the left operand's width — the shift
        // amount must not widen the result.
        let w = if matches!(op, Shl | Shr | Sar | Rol | Ror) {
            self.bits.len().max(1)
        } else {
            self.bits.len().max(rhs.bits.len()).max(1)
        };
        match op {
            Eq | Ne | EqStrict | NeStrict => {
                let strict = matches!(op, EqStrict | NeStrict);
                let w = self.bits.len().max(rhs.bits.len());
                let mut all_eq = true;
                let mut any_unknown = false;
                // Operands are zero-extended to the wider width (unsigned
                // semantics: reg/wire/integer are all unsigned here).
                for i in 0..w {
                    let a = if i < self.bits.len() {
                        self.bits[i]
                    } else {
                        V0
                    };
                    let b = if i < rhs.bits.len() { rhs.bits[i] } else { V0 };
                    if strict {
                        if a != b {
                            all_eq = false;
                        }
                    } else {
                        if !is_known(a) || !is_known(b) {
                            any_unknown = true;
                        } else if a != b {
                            all_eq = false;
                        }
                    }
                }
                if !strict && any_unknown {
                    return Value::x(1);
                }
                if !strict && !all_eq {
                    // String-aware fallback: `to_bstring(v) = "1010"` compares a
                    // byte-packed char string against a bit-vector literal.
                    all_eq = eq_string_loose(self, rhs);
                }
                let r = match op {
                    Eq | EqStrict => all_eq,
                    _ => !all_eq,
                };
                Value::from_u64(1, r as u64)
            }
            Lt | Le | Gt | Ge => {
                let (a, b) = (self.to_u64(), rhs.to_u64());
                let ord = match (a, b) {
                    (Some(a), Some(b)) => match op {
                        Lt => a < b,
                        Le => a <= b,
                        Gt => a > b,
                        _ => a >= b,
                    },
                    _ => return Value::x(1),
                };
                Value::from_u64(1, ord as u64)
            }
            EqM => return Value::from_u64(1, self.matches_case(rhs) as u64),
            NeM => return Value::from_u64(1, !self.matches_case(rhs) as u64),
            LtM | LeM | GtM | GeM => {
                // Matching relational: '-' would need wildcard-aware ordering;
                // conservatively X when either side has a don't-care bit.
                if self.bits.iter().any(|&b| b == VDASH) || rhs.bits.iter().any(|&b| b == VDASH) {
                    return Value::x(1);
                }
                let (a, b) = (self.to_u64(), rhs.to_u64());
                let ord = match (a, b) {
                    (Some(a), Some(b)) => match op {
                        LtM => a < b,
                        LeM => a <= b,
                        GtM => a > b,
                        _ => a >= b,
                    },
                    _ => return Value::x(1),
                };
                return Value::from_u64(1, ord as u64);
            }
            LogAnd | LogOr => {
                // Logical: X if any operand bit unknown, else 0/1.
                let truth = |v: &Value| -> u8 {
                    if v.bits.iter().any(|&b| !is_known(b)) {
                        VX
                    } else if v.to_u64() == Some(0) {
                        V0
                    } else {
                        V1
                    }
                };
                let (a, b) = (truth(self), truth(rhs));
                let r = match op {
                    LogAnd => and1(a, b),
                    _ => or1(a, b),
                };
                Value::from_bit(r)
            }
            _ => {
                let mut bits = vec![VX; w];
                for i in 0..w {
                    let a = self.bit(i);
                    let b = rhs.bit(i);
                    bits[i] = bit_op(op, a, b);
                }
                // Arithmetic needs full-width evaluation, not per-bit.
                match op {
                    Add | Sub | Mul | Div | Mod | Pow | Shl | Shr | Sar | Rol | Ror => {
                        return self.arith(op, rhs, w);
                    }
                    _ => {}
                }
                Value { bits, real: false }
            }
        }
    }

    fn arith(&self, op: BinOp, rhs: &Value, w: usize) -> Value {
        use BinOp::*;
        let a = self.to_u64();
        let b = rhs.to_u64();
        match (a, b) {
            (Some(a), Some(b)) => {
                let r = match op {
                    Add => a.wrapping_add(b),
                    Sub => a.wrapping_sub(b),
                    Mul => a.wrapping_mul(b),
                    Div => {
                        if b == 0 {
                            return Value::x(w);
                        }
                        a / b
                    }
                    Mod => {
                        if b == 0 {
                            return Value::x(w);
                        }
                        a % b
                    }
                    Pow => {
                        if b > 63 {
                            u64::MAX
                        } else {
                            a.checked_pow(b as u32).unwrap_or(u64::MAX)
                        }
                    }
                    Shl => a.wrapping_shl(b as u32),
                    Shr => a.wrapping_shr(b as u32),
                    Rol | Ror => {
                        let m = if w >= 64 { u64::MAX } else { (1u64 << w) - 1 };
                        let a = a & m;
                        if w >= 64 {
                            if matches!(op, Rol) {
                                a.rotate_left((b as u32) % 64)
                            } else {
                                a.rotate_right((b as u32) % 64)
                            }
                        } else {
                            let sh = (b as u32) % (w as u32);
                            if sh == 0 {
                                a
                            } else if matches!(op, Rol) {
                                ((a << sh) | (a >> (w as u32 - sh))) & m
                            } else {
                                ((a >> sh) | (a << (w as u32 - sh))) & m
                            }
                        }
                    }
                    // Arithmetic: sign-extend to i64, shift, keep low w bits.
                    Sar => {
                        let sh = (b as u32) % 64;
                        let sa: u64 = if w >= 64 {
                            ((a as i64) >> sh) as u64
                        } else if (a >> (w - 1)) & 1 == 1 {
                            // negative: sign-extend then shift
                            let ext = (a | (u64::MAX << w)) as i64;
                            (ext >> sh) as u64
                        } else {
                            a >> sh
                        };
                        sa & (if w >= 64 { u64::MAX } else { (1u64 << w) - 1 })
                    }
                    _ => unreachable!("arith: non-arith op"),
                };
                Value::from_u64(w, r)
            }
            _ => Value::x(w),
        }
    }

    pub fn unop(&self, op: UnOp) -> Value {
        use UnOp::*;
        match op {
            // Logical not: 1-bit result, 1 iff the operand is known zero.
            Not => {
                if !self.is_known() {
                    Value::x(1)
                } else {
                    Value::from_bit(if self.to_u64() == Some(0) { V1 } else { V0 })
                }
            }
            BNot => {
                let mut bits = vec![VX; self.bits.len().max(1)];
                for (j, b) in bits.iter_mut().enumerate() {
                    *b = bnot1(self.bit(j));
                }
                Value { bits, real: false }
            }
            // Reductions produce 1 bit; fold with the operator's identity
            // (AND/XNOR seed 1, OR/XOR seed 0), then invert for the negated
            // forms.
            RedAnd | RedNand => {
                let acc = self.bits.iter().copied().fold(V1, |acc, b| and1(acc, b));
                Value::from_bit(if matches!(op, RedNand) {
                    bnot1(acc)
                } else {
                    acc
                })
            }
            RedOr | RedNor => {
                let acc = self.bits.iter().copied().fold(V0, |acc, b| or1(acc, b));
                Value::from_bit(if matches!(op, RedNor) {
                    bnot1(acc)
                } else {
                    acc
                })
            }
            RedXor | RedXnor => {
                let acc = self.bits.iter().copied().fold(V0, |acc, b| xor1(acc, b));
                Value::from_bit(if matches!(op, RedXnor) {
                    bnot1(acc)
                } else {
                    acc
                })
            }
            Neg | Pos => {
                if op == Neg && self.is_known() {
                    if let Some(v) = self.to_u64() {
                        return Value::from_u64(self.bits.len(), v.wrapping_neg());
                    }
                }
                if !self.is_known() {
                    return Value::x(self.bits.len());
                }
                self.clone()
            }
            Abs => {
                if !self.is_known() {
                    return Value::x(self.bits.len());
                }
                let w = self.bits.len();
                let n = self.to_u64().unwrap_or(0);
                let x = if w < 64 && (n >> (w - 1)) & 1 == 1 {
                    (n | (u64::MAX << w)) as i64
                } else {
                    n as i64
                };
                Value::from_u64(w, x.unsigned_abs())
            }
        }
    }

    /// Render for $display-style output: %h hex, %d decimal, %b binary, %o octal,
    /// %c char, %s string, %t time. X/Z shown as x/z.
    pub fn display(&self, fmt: &str) -> String {
        match fmt {
            "h" | "x" => {
                let mut s = String::new();
                let nibbles = (self.bits.len() + 3) / 4;
                for n in (0..nibbles).rev() {
                    let mut v = 0u8;
                    let mut unknown = false;
                    for j in 0..4 {
                        let b = self.bit(n * 4 + j);
                        if is_known(b) {
                            v |= ((b == V1) as u8) << j;
                        } else {
                            unknown = true;
                        }
                    }
                    s.push(if unknown {
                        'x'
                    } else {
                        std::char::from_digit(v as u32, 16).unwrap_or('?')
                    });
                }
                s
            }
            "b" => self
                .bits
                .iter()
                .rev()
                .map(|&b| match b {
                    V1 => '1',
                    V0 => '0',
                    VZ => 'z',
                    _ => 'x',
                })
                .collect(),
            "d" => match self.to_u64() {
                Some(v) => v.to_string(),
                None => "x".repeat((self.bits.len() + 3) / 4),
            },
            "o" => {
                let mut s = String::new();
                let groups = (self.bits.len() + 2) / 3;
                for g in (0..groups).rev() {
                    let mut v = 0u8;
                    let mut unknown = false;
                    for j in 0..3 {
                        let b = self.bit(g * 3 + j);
                        if is_known(b) {
                            v |= ((b == V1) as u8) << j;
                        } else {
                            unknown = true;
                        }
                    }
                    s.push(if unknown {
                        'x'
                    } else {
                        std::char::from_digit(v as u32, 8).unwrap_or('?')
                    });
                }
                s
            }
            "c" => match self.to_u64() {
                Some(v) if (v as u8).is_ascii() => ((v as u8) as char).to_string(),
                _ => "?".to_string(),
            },
            _ => format!("<{}>", fmt),
        }
    }
}

// ---- per-bit 4-state logic tables (0/1/X/Z propagate as X) -------------------

fn lift2(f: impl Fn(bool, bool) -> bool, a: u8, b: u8) -> u8 {
    if is_known(a) && is_known(b) {
        if f(a == V1, b == V1) {
            V1
        } else {
            V0
        }
    } else {
        VX
    }
}

fn and1(a: u8, b: u8) -> u8 {
    if a == V0 || b == V0 {
        V0
    } else {
        lift2(|x, y| x && y, a, b)
    }
}
fn or1(a: u8, b: u8) -> u8 {
    if a == V1 || b == V1 {
        V1
    } else {
        lift2(|x, y| x || y, a, b)
    }
}
fn xor1(a: u8, b: u8) -> u8 {
    lift2(|x, y| x ^ y, a, b)
}
fn xnor1(a: u8, b: u8) -> u8 {
    lift2(|x, y| !(x ^ y), a, b)
}
fn nand1(a: u8, b: u8) -> u8 {
    bnot1(and1(a, b))
}
fn nor1(a: u8, b: u8) -> u8 {
    bnot1(or1(a, b))
}
fn bnot1(a: u8) -> u8 {
    if is_known(a) {
        if a == V1 {
            V0
        } else {
            V1
        }
    } else {
        VX
    }
}
fn bit_op(op: BinOp, a: u8, b: u8) -> u8 {
    match op {
        BinOp::BAnd => and1(a, b),
        BinOp::BOr => or1(a, b),
        BinOp::BXor => xor1(a, b),
        BinOp::BXnor => xnor1(a, b),
        BinOp::BNand => nand1(a, b),
        BinOp::BNor => nor1(a, b),
        _ => VX,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    /// Exponentiation `**` (VHDL and Verilog); saturates on overflow.
    Pow,
    Shl,
    Shr,
    /// Arithmetic shift right (sign-preserving) — VHDL `sra`.
    Sar,
    /// Rotate left within the operand width — VHDL `rol`.
    Rol,
    /// Rotate right within the operand width — VHDL `ror`.
    Ror,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    EqStrict,
    NeStrict,
    /// VHDL-2008 matching equality `?=` — '-' bits in either operand are
    /// wildcards.
    EqM,
    /// VHDL-2008 matching inequality `?/= `.
    NeM,
    /// VHDL-2008 matching relational operators (`?<`, `?<=`, `?>`, `?>=`).
    /// Numeric compare when neither side has '-', else X.
    LtM,
    LeM,
    GtM,
    GeM,
    BAnd,
    BOr,
    BXor,
    BNand,
    BNor,
    BXnor,
    LogAnd,
    LogOr,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
pub enum UnOp {
    Not,
    BNot,
    Neg,
    Pos,
    RedAnd,
    RedOr,
    RedXor,
    RedNand,
    RedNor,
    RedXnor,
    /// VHDL `abs x` — absolute value (two's complement).
    Abs,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_hex_for_clean_values_msb_first() {
        assert_eq!(
            serde_json::to_string(&Value::from_u64(8, 0xA5)).unwrap(),
            r#"{"w":8,"hex":"a5"}"#
        );
        assert_eq!(
            serde_json::to_string(&Value::from_u64(12, 0xABC)).unwrap(),
            r#"{"w":12,"hex":"abc"}"#
        );
        // Width not a multiple of 4: the top digit holds the leftover bits.
        assert_eq!(
            serde_json::to_string(&Value::from_u64(6, 0b11_1111)).unwrap(),
            r#"{"w":6,"hex":"f3"}"#
        );
        assert_eq!(
            serde_json::to_string(&Value {
                bits: vec![V1],
                real: false,
            })
            .unwrap(),
            r#"{"w":1,"hex":"1"}"#
        );
    }

    #[test]
    fn json_bits_string_for_x_z_and_metavalues() {
        // LSB-first [1,0,x,z] reads MSB-first as "zx01".
        let v = Value {
            bits: vec![V1, V0, VX, VZ],
            real: false,
        };
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            r#"{"w":4,"bits":"zx01"}"#
        );
        // Weak and metavalue states keep their own letters — no aliasing to
        // strong 0/1 (W must not look low, U must not look like a zero init).
        let v = Value {
            bits: vec![VW, VL, VH, VDASH, VU, V0, V1, VZ],
            real: false,
        };
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            r#"{"w":8,"bits":"z10u-hlw"}"#
        );
        assert_eq!(
            serde_json::to_string(&Value {
                bits: vec![VZ],
                real: false,
            })
            .unwrap(),
            r#"{"w":1,"bits":"z"}"#
        );
    }

    #[test]
    fn fmt_value_binary_hex_real_and_meta() {
        // Narrow clean values read as binary (bit positions visible).
        assert_eq!(Value::from_u64(4, 0b0001).fmt_value(), "0b0001");
        assert_eq!(Value::from_u64(8, 0).fmt_value(), "0b00000000");
        // Wider clean values read as hex; leading zero nibbles collapse.
        assert_eq!(Value::from_u64(16, 0x00ff).fmt_value(), "0xff");
        assert_eq!(Value::from_u64(16, 0x1000).fmt_value(), "0x1000");
        assert_eq!(Value::from_u64(12, 0xf).fmt_value(), "0xf");
        assert_eq!(Value::from_u64(10, 0b11_1111).fmt_value(), "0xf3");
        assert_eq!(Value::from_u64(12, 0).fmt_value(), "0x0");
        // Real-typed values decode the f64.
        let mut r = Value::from_u64(64, 0);
        r.real = true;
        assert_eq!(r.fmt_value(), "r=0");
        let mut one = Value::from_u64(64, 1.5f64.to_bits());
        one.real = true;
        assert_eq!(one.fmt_value(), "r=1.5");
        // Metavalues print one state letter per bit, MSB first, quoted.
        let m = Value {
            bits: vec![V1, V0, VX, VZ],
            real: false,
        };
        assert_eq!(m.fmt_value(), r#""zx01""#);
    }

    #[test]
    fn has_meta_distinguishes_clean_from_unknown() {
        assert!(!Value::from_u64(4, 0b1010).has_meta());
        assert!(Value {
            bits: vec![V0, VW, V1],
            real: false,
        }
        .has_meta());
        assert!(!Value::from_u64(1, 0).has_meta());
    }

    #[test]
    fn from_u64_roundtrip() {
        let v = Value::from_u64(8, 0xA5);
        assert_eq!(v.to_u64(), Some(0xA5));
        assert_eq!(v.bit(0), V1);
        assert_eq!(v.bit(1), V0);
        assert_eq!(v.bit(7), V1);
    }

    #[test]
    fn to_i64_sign_extends_from_width() {
        // A width-32 integer holding -1 must read as -1, not 4294967295:
        // range-bound arithmetic (`0 to N-1` with N=0) depends on it.
        assert_eq!(Value::from_u64(32, u32::MAX as u64).to_i64(), Some(-1));
        assert_eq!(Value::from_u64(32, 5).to_i64(), Some(5));
        assert_eq!(Value::from_u64(8, 0xFF).to_i64(), Some(-1));
        assert_eq!(Value::from_u64(64, u64::MAX).to_i64(), Some(-1));
        assert_eq!(Value::x(4).to_i64(), None);
    }

    #[test]
    fn x_propagates_through_and() {
        let a = Value::from_u64(4, 0b1100);
        let b = Value::x(4);
        let r = a.binop(BinOp::BAnd, &b);
        assert_eq!(r.bit(0), V0); // 0 & x = 0
        assert_eq!(r.bit(2), VX); // 1 & x = x
    }

    #[test]
    fn strict_eq_compares_x() {
        let a = Value::x(4);
        let b = Value::x(4);
        assert_eq!(a.binop(BinOp::EqStrict, &b).to_u64(), Some(1));
        let c = Value::from_u64(4, 0);
        assert_eq!(a.binop(BinOp::EqStrict, &c).to_u64(), Some(0));
    }

    #[test]
    fn nonstrict_eq_with_x_is_x() {
        let a = Value::x(4);
        let c = Value::from_u64(4, 0);
        let r = a.binop(BinOp::Eq, &c);
        assert_eq!(r.to_u64(), None); // X result
    }

    #[test]
    fn arithmetic_and_resize() {
        let a = Value::from_u64(8, 200);
        let b = Value::from_u64(8, 100);
        assert_eq!(a.binop(BinOp::Add, &b).to_u64(), Some(44)); // wraps at 8 bits
        let w = a.resize(16);
        assert_eq!(w.to_u64(), Some(200));
        assert_eq!(w.width(), 16);
    }

    #[test]
    fn concat_and_slice() {
        let a = Value::from_u64(4, 0xA);
        let b = Value::from_u64(4, 0x5);
        let c = Value::concat(&[a, b]); // a is MSB
        assert_eq!(c.to_u64(), Some(0xA5));
        let s = c.slice(3, 0);
        assert_eq!(s.to_u64(), Some(0x5));
    }

    #[test]
    fn f64_roundtrip() {
        for f in [0.0, 1.5, -2.25, 7.5, 1e300, 1e-300, 3.141592653589793] {
            let v = Value::from_f64(f);
            assert_eq!(v.width(), 64);
            assert_eq!(v.to_f64(), Some(f));
        }
        assert_eq!(Value::x(64).to_f64(), None);
        assert_eq!(Value::from_u64(32, 5).to_f64(), None);
    }

    #[test]
    fn pow() {
        let two = Value::from_u64(32, 2);
        let three = Value::from_u64(32, 3);
        assert_eq!(two.binop(BinOp::Pow, &three).to_u64(), Some(8));
        assert_eq!(three.binop(BinOp::Pow, &two).to_u64(), Some(9));
        assert_eq!(
            Value::from_u64(32, 5)
                .binop(BinOp::Pow, &Value::from_u64(32, 0))
                .to_u64(),
            Some(1)
        );
        // Overflow saturates (then truncates to the 32-bit operand width).
        assert_eq!(
            two.binop(BinOp::Pow, &Value::from_u64(32, 100)).to_u64(),
            Some(u32::MAX as u64)
        );
        // Unknown exponent → X.
        assert_eq!(two.binop(BinOp::Pow, &Value::x(32)).to_u64(), None);
    }

    #[test]
    fn display_hex_and_bin() {
        let v = Value::from_u64(8, 0xA5);
        assert_eq!(v.display("h"), "a5");
        assert_eq!(v.display("b"), "10100101");
        let x = Value::x(4);
        assert_eq!(x.display("h"), "x");
    }

    #[test]
    fn shifts() {
        let a = Value::from_u64(8, 1);
        let b = Value::from_u64(8, 3);
        assert_eq!(a.binop(BinOp::Shl, &b).to_u64(), Some(8));
        let c = Value::from_u64(8, 8);
        assert_eq!(c.binop(BinOp::Shr, &b).to_u64(), Some(1));
    }

    #[test]
    fn rotates_stay_within_operand_width() {
        let v = Value::from_u64(8, 0x0F);
        let n = Value::from_u64(32, 4); // wide amount must not widen the result
        let rol = v.binop(BinOp::Rol, &n);
        let ror = v.binop(BinOp::Ror, &n);
        assert_eq!(rol.width(), 8);
        assert_eq!(rol.to_u64(), Some(0xF0));
        assert_eq!(ror.to_u64(), Some(0xF0));
        let one = Value::from_u64(32, 1);
        assert_eq!(v.binop(BinOp::Ror, &one).to_u64(), Some(0x87));
        // Arithmetic shift right sign-extends within the width.
        let s = Value::from_u64(8, 0xF0);
        assert_eq!(s.binop(BinOp::Sar, &one).to_u64(), Some(0xF8));
    }
}
