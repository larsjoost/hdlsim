//! VHDL literal lowering: physical literals (`100 ps`), bit-string literals
//! (`x"FF"`, `32SB"1"`), character literals, string literals, and numeric
//! literals. Split out of `expr.rs` to keep every function at its standards
//! baseline; the entry point keeps the `lower_literal` name and call sites.

use super::*;

impl<'a> Lower<'a> {
    /// Lower a literal node by dispatching on its textual form: physical,
    /// bit-string, character, string, numeric, `null`.
    pub(super) fn lower_literal(&mut self, e: &SyntaxNode, env: &HashMap<String, Value>) -> Expr {
        let t = self.text(e);
        if let Some(v) = self.physical_literal(&t) {
            return v;
        }
        let t = strip_bit_string_prefix(&t);
        if let Some(v) = self.bit_string_value(t) {
            return v;
        }
        if let Some(v) = self.char_literal_value(t) {
            return v;
        }
        if let Some(v) = self.string_literal_value(t) {
            return v;
        }
        if let Some(v) = self.numeric_literal(e, t) {
            return v;
        }
        let _ = env;
        // `null` — the null access value (access types are not simulated).
        if t.eq_ignore_ascii_case("null") {
            return Expr::Lit(Value::x(64));
        }
        self.err(e.span(), format!("unsupported literal '{}'", t));
        Expr::Lit(Value::x(1))
    }

    /// Physical literal: `100 ps` — the parser merges number + unit into
    /// one Literal; the value is femtoseconds. `None` keeps the remaining
    /// literal forms.
    fn physical_literal(&mut self, t: &str) -> Option<Expr> {
        let (num, unit) = t.split_once(char::is_whitespace)?;
        if matches!(
            unit.trim().to_ascii_lowercase().as_str(),
            "fs" | "ps"
                | "ns"
                | "us"
                | "ms"
                | "sec"
                | "s"
                | "min"
                | "hr"
                | "um"
                | "hz"
                | "khz"
                | "mhz"
                | "ghz"
        ) {
            // Integer physical (`100 ps`) or real physical (`2.5 ns`,
            // VHDL-2008) \u2014 the value is femtoseconds.
            let fs = match self.parse_int_literal(num.trim()).and_then(|v| v.to_u64()) {
                // saturating: huge literals (`1e18 hr`-scale constants) must
                // not overflow in debug builds
                Some(n) => n.saturating_mul(time_unit_fs(unit.trim())),
                None => num
                    .trim()
                    .parse::<f64>()
                    .map(|f| (f * time_unit_fs(unit.trim()) as f64) as u64)
                    .unwrap_or(0),
            };
            // Time is femtoseconds — 64-bit, else `30 ms` overflows 32 bits.
            return Some(Expr::Lit(Value::from_u64(64, fs)));
        }
        // User-declared physical unit (`1.5 cm` of a `units ... end units`
        // type): value in the type's base unit, 64-bit.
        let v = self.phys_lit_value(num, unit.trim())?;
        if !self
            .phys_units
            .contains_key(&unit.trim().to_ascii_lowercase())
        {
            return None;
        }
        Some(Expr::Lit(Value::from_u64(64, v)))
    }

    /// Bit string literal: x"FF" / b"01" / o"7", or sized `32X"0"` / signed
    /// `32SB"1"` / unsigned `32UB"1"` (VHDL-2008/2019). `None` when `t` is
    /// not a bit string.
    fn bit_string_value(&self, t: &str) -> Option<Expr> {
        if !(t.len() >= 2
            && matches!(
                t.as_bytes()[0],
                b'x' | b'X' | b'b' | b'B' | b'o' | b'O' | b'd' | b'D'
            )
            && t.as_bytes()[1] == b'"')
        {
            return None;
        }
        let base = match t.as_bytes()[0].to_ascii_lowercase() {
            b'x' => 16,
            b'o' => 8,
            b'd' => 10,
            _ => 2,
        };
        let digits = t[2..t.len() - 1].replace('_', "");
        let digit_bits = match base {
            16 => 4,
            10 => 4, // decimal digit in a D bit string: 4 bits (BCD-style)
            8 => 3,
            _ => 1,
        };
        let width = digits.len() * digit_bits;
        let mut bits = vec![VX; width.max(1)];
        for (i, c) in digits.chars().enumerate() {
            let from_lsb = (digits.len() - 1 - i) * digit_bits;
            match c.to_ascii_lowercase() {
                'x' | '?' => {}
                'z' => {
                    for j in 0..digit_bits {
                        let idx = from_lsb + j;
                        if idx < bits.len() {
                            bits[idx] = VZ;
                        }
                    }
                }
                _ => {
                    if let Some(d) = c.to_digit(base) {
                        for j in 0..digit_bits {
                            let idx = from_lsb + j;
                            if idx < bits.len() {
                                bits[idx] = if (d >> j) & 1 == 1 { V1 } else { V0 };
                            }
                        }
                    }
                }
            }
        }
        Some(self.lit_vec_mark(Value { bits, real: false }))
    }

    /// Character literal: the std_logic alphabet keeps its 1-bit value
    /// ('0'/'1'/'z', the rest X); any other character is an 8-bit ASCII
    /// `character` value. `None` when `t` is not a character literal.
    fn char_literal_value(&self, t: &str) -> Option<Expr> {
        if !(t.starts_with('\'') && t.ends_with('\'') && t.len() == 3) {
            return None;
        }
        let c = t.as_bytes()[1];
        let b = match c.to_ascii_lowercase() {
            b'0' => V0,
            b'1' => V1,
            b'z' => VZ,
            b'u' => VU,
            b'-' | b'l' | b'h' | b'w' | b'x' => VX,
            _ => {
                return Some(Expr::Lit(Value {
                    bits: (0..8)
                        .map(|j| if (c >> j) & 1 == 1 { V1 } else { V0 })
                        .collect(),
                    real: false,
                }));
            }
        };
        Some(Expr::Lit(Value {
            bits: vec![b],
            real: false,
        }))
    }

    /// String literal: a string of bit characters is a std_logic_vector
    /// value (`"0000"`), unless the literal sits in a string-typed object's
    /// initializer, where the characters are ASCII text. `None` when `t` is
    /// not a string literal.
    fn string_literal_value(&self, t: &str) -> Option<Expr> {
        if !t.starts_with('"') {
            return None;
        }
        let inner = t.trim_matches('"').replace("\"\"", "\"");
        if !self.str_ctx && is_std_logic_text(&inner) {
            return Some(self.std_logic_string_value(&inner));
        }
        Some(self.text_string_value(&inner))
    }

    /// 1-bit-per-character std_logic_vector value.
    fn std_logic_string_value(&self, inner: &str) -> Expr {
        let mut bits = vec![VX; inner.len()];
        for (i, c) in inner.bytes().enumerate() {
            bits[inner.len() - 1 - i] = match c.to_ascii_lowercase() {
                b'0' => V0,
                b'1' => V1,
                b'z' => VZ,
                b'u' => VU,
                b'w' => VW,
                b'l' => VL,
                b'h' => VH,
                b'-' | b'?' => VDASH, // don't-care, matched by `case?`
                _ => VX,
            };
        }
        self.lit_vec_mark(Value { bits, real: false })
    }

    /// ASCII text value: first char in the high byte (MSB-first), so
    /// `&`-concatenation and string_of read left to right. An empty
    /// literal is zero-width — `'length` of the formal it feeds must
    /// be 0, or a guarded `result(1 to arg'length) := arg` runs with
    /// a phantom character and clobbers the first result char.
    fn text_string_value(&self, inner: &str) -> Expr {
        let bytes = inner.as_bytes();
        let n = bytes.len();
        let mut bits = vec![V0; n * 8];
        for (bi, &b) in bytes.iter().enumerate() {
            for j in 0..8 {
                bits[(n - 1 - bi) * 8 + j] = if (b >> j) & 1 == 1 { V1 } else { V0 };
            }
        }
        self.lit_vec_mark(Value { bits, real: false })
    }

    /// Numeric literal: universal integer, or a real (`2.71828_...`,
    /// VHDL digit separators stripped first). `None` for anything else.
    fn numeric_literal(&mut self, e: &SyntaxNode, t: &str) -> Option<Expr> {
        if let Some(v) = self.parse_int_literal(t) {
            return Some(Expr::Lit(v));
        }
        // VHDL digit separators (`2.71828_18284_...`) are not valid in Rust's
        // f64 parse — strip them first.
        let t_clean: String = t.chars().filter(|c| *c != '_').collect();
        if let Ok(v) = t_clean.parse::<f64>() {
            // VHDL real literal (contains '.' or exponent, no base '#').
            if t_clean.contains('.') || t_clean.contains('e') || t_clean.contains('E') {
                return Some(Expr::Real(v));
            }
            self.warn(e.span(), "real literals unsupported; X");
            return Some(Expr::Lit(Value::x(32)));
        }
        None
    }
}

/// A string of std_logic metavalues lowers 1 bit per character — unless it
/// sits in a string-typed context (checked by the caller) or reads as a
/// one-character report text. A lone letter ("h") is far more often the
/// one-character STRING "h" than a 1-bit weak value — u/w/l/h spell words in
/// report text and filenames. Only a multi-character literal ("UWH-") reads
/// as std_ulogic metavalues; a single one is written 'h' (char literal,
/// handled above).
fn is_std_logic_text(inner: &str) -> bool {
    !inner.is_empty()
        && inner.bytes().all(|c| {
            matches!(
                c,
                b'0' | b'1'
                    | b'x'
                    | b'X'
                    | b'z'
                    | b'Z'
                    | b'u'
                    | b'U'
                    | b'w'
                    | b'W'
                    | b'l'
                    | b'L'
                    | b'h'
                    | b'H'
                    | b'-'
                    | b'?'
            )
        })
        && !(inner.len() == 1
            && inner
                .bytes()
                .next()
                .is_some_and(|c| matches!(c.to_ascii_lowercase(), b'u' | b'w' | b'l' | b'h')))
}

/// Strip the leading decimal size and optional `S`/`U` prefix of a bit
/// string literal (`32SB"1"` → `"1"`); an unsized literal is unchanged.
fn strip_bit_string_prefix(t: &str) -> &str {
    let digits = t.len() - t.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let mut n = digits;
    if digits > 0 && t.len() > n && matches!(t.as_bytes()[n], b's' | b'S' | b'u' | b'U') {
        n += 1;
    }
    if n > 0
        && t.len() > n
        && matches!(
            t.as_bytes()[n],
            b'x' | b'X' | b'b' | b'B' | b'o' | b'O' | b'd' | b'D'
        )
    {
        &t[n..]
    } else {
        t
    }
}
