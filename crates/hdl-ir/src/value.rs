//! Multi-state logic values — the shared data model of the simulator.
//!
//! Each bit is one of the nine VHDL `std_logic` states; Verilog uses the 4-state
//! subset (`0`, `1`, `X`, `Z`). Bits are stored LSB-first: `bits[0]` is bit 0.
//!
//! Invariants:
//! - `bits[i]` is always one of the `V*` constants below.
//! - Arithmetic/bitwise results propagate X/Z: if any operand bit is not 0/1,
//!   the corresponding result bit is X (except `===`/`!==`, which compare X/Z).

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

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Value {
    pub bits: Vec<u8>,
}

impl Value {
    pub fn new(width: usize, fill: u8) -> Value {
        Value {
            bits: vec![fill; width.max(1)],
        }
    }

    pub fn from_u64(width: usize, v: u64) -> Value {
        let mut bits = vec![V0; width.max(1)];
        for i in 0..width.min(64) {
            bits[i] = if (v >> i) & 1 == 1 { V1 } else { V0 };
        }
        Value { bits }
    }

    /// All-X value of the given width.
    pub fn x(width: usize) -> Value {
        Value::new(width, VX)
    }

    /// Single-bit value from a bit state constant (`V0`, `V1`, `VX`, …).
    pub fn from_bit(b: u8) -> Value {
        Value { bits: vec![b] }
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

    pub fn is_known(&self) -> bool {
        self.bits.iter().all(|&b| is_known(b))
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
        Value { bits }
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
        Value { bits }
    }

    /// Slice `bits[lsb..=msb]` (inclusive, Verilog `[msb:lsb]`).
    pub fn slice(&self, msb: usize, lsb: usize) -> Value {
        let msb = msb.min(self.bits.len().saturating_sub(1));
        let lsb = lsb.min(msb);
        Value {
            bits: self.bits[lsb..=msb].to_vec(),
        }
    }

    /// Bitwise/logical op with X/Z propagation. `strict` ops (===, !==) compare
    /// X/Z literally and never produce X.
    pub fn binop(&self, op: BinOp, rhs: &Value) -> Value {
        use BinOp::*;
        let w = self.bits.len().max(rhs.bits.len()).max(1);
        match op {
            Eq | Ne | EqStrict | NeStrict => {
                let strict = matches!(op, EqStrict | NeStrict);
                let w = self.bits.len().max(rhs.bits.len());
                let mut all_eq = true;
                let mut any_unknown = false;
                for i in 0..w {
                    let a = self.bit(i);
                    let b = rhs.bit(i);
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
                    Add | Sub | Mul | Div | Mod | Shl | Shr => {
                        return self.arith(op, rhs, w);
                    }
                    _ => {}
                }
                Value { bits }
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
                    Shl => a.wrapping_shl(b as u32),
                    Shr => a.wrapping_shr(b as u32),
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
            Not | BNot | RedNand | RedNor | RedXnor => {
                let mut bits = vec![VX; self.bits.len().max(1)];
                for i in 0..bits.len() {
                    bits[i] = match op {
                        Not | BNot => bnot1(self.bit(i)),
                        _ => VX,
                    };
                }
                if matches!(op, RedNand | RedNor | RedXnor) {
                    // Reduction ops produce 1 bit.
                    let acc = self.bits.iter().copied().fold(V0, |acc, b| match op {
                        RedNand => nand1(acc, b),
                        RedNor => nor1(acc, b),
                        RedXnor => xnor1(acc, b),
                        _ => acc,
                    });
                    return Value::from_bit(acc);
                }
                Value { bits }
            }
            RedAnd | RedOr | RedXor => {
                let acc = self.bits.iter().copied().fold(V0, |acc, b| match op {
                    RedAnd => and1(acc, b),
                    RedOr => or1(acc, b),
                    _ => xor1(acc, b),
                });
                Value::from_bit(acc)
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Shl,
    Shr,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    EqStrict,
    NeStrict,
    BAnd,
    BOr,
    BXor,
    BNand,
    BNor,
    BXnor,
    LogAnd,
    LogOr,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_u64_roundtrip() {
        let v = Value::from_u64(8, 0xA5);
        assert_eq!(v.to_u64(), Some(0xA5));
        assert_eq!(v.bit(0), V1);
        assert_eq!(v.bit(1), V0);
        assert_eq!(v.bit(7), V1);
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
}
