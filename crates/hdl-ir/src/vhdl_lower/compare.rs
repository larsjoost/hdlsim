//! Comparison lowering: ordered-comparison signedness marking and the
//! `ieee.std_logic_arith` mixed-comparison semantics. A comparison between
//! an `unsigned`/`signed` vector and an integer wraps the integer operand
//! to a vector before comparing (probed vendor behavior); every other
//! comparison keeps the kernel's extend-to-wider evaluation.

use super::*;

/// Operand class of one side of a comparison for mixed-comparison
/// wrapping (the expression-level twin of the per-signal [`ObjClass`]):
/// the arith arm carries the wrap target (width + signedness).
enum CmpOperand {
    /// `unsigned`/`signed` vector: wraps the integer side to this width.
    Arith { signed: bool, width: usize },
    /// Integer scalar: the side that gets wrapped.
    Integer,
    /// Not confidently classified: comparison untouched.
    Other,
}

impl<'a> Lower<'a> {
    /// Whether a lowered expression is signed by its declared type: the
    /// `Signed`/`Resize` markers, or a signal/slot whose declaration recorded
    /// signedness (integer objects). Mirrors the kernel's `expr_signed`.
    pub(super) fn expr_is_signed(&self, e: &Expr) -> bool {
        match e {
            Expr::Signed(_) => true,
            Expr::Resize { signed, .. } => *signed,
            Expr::Sig(i) => self.sig_signed.get(i).copied().unwrap_or(false),
            Expr::Bin { op, l, r } => {
                if matches!(
                    op,
                    BinOp::Shl | BinOp::Shr | BinOp::Sar | BinOp::Rol | BinOp::Ror
                ) {
                    self.expr_is_signed(l)
                } else {
                    self.expr_is_signed(l) && self.expr_is_signed(r)
                }
            }
            Expr::Un { e, .. } => self.expr_is_signed(e),
            _ => false,
        }
    }

    /// Build an ordered comparison (`<`, `<=`, `>`, `>=`): when either
    /// operand is signed by its declared type, mark both so the comparison
    /// sign-extends. Kernel slot reads (subprogram variables) default to
    /// unsigned, and the 1076.2 UNIFORM seed fix-up (`if TSEED2 < 0`) must
    /// see a negative seed, not 4294967295. Other operators keep their
    /// operands untouched (two's-complement arithmetic is bit-identical).
    pub(super) fn mark_signed_compare(&self, op: BinOp, l: Expr, r: Expr) -> Expr {
        // Predefined equality is a value comparison (LRM 7.2) — but only the
        // scalar shape stays strict here, where `'U' = '0'` must be false and
        // never X. Wide (composite) operands keep the loose ops: their
        // metavalues stay unknown (an X condition takes no action, and the
        // string-shape reconciliation lives on the loose path).
        let op = if matches!(op, BinOp::EqStrict | BinOp::NeStrict)
            && (self.expr_width_est(&l) > 1 || self.expr_width_est(&r) > 1)
        {
            match op {
                BinOp::EqStrict => BinOp::Eq,
                _ => BinOp::Ne,
            }
        } else {
            op
        };
        let (l, r) = self.wrap_mixed_compare(op, l, r);
        if matches!(op, BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge)
            && (self.expr_is_signed(&l) || self.expr_is_signed(&r))
        {
            return Expr::Bin {
                op,
                l: Box::new(Expr::Signed(Box::new(l))),
                r: Box::new(Expr::Signed(Box::new(r))),
            };
        }
        Expr::Bin {
            op,
            l: Box::new(l),
            r: Box::new(r),
        }
    }

    /// std_logic_arith mixed comparison (`unsigned/signed` vs `integer`):
    /// the vendor converts the integer operand to a vector and compares
    /// same-width, so the integer wraps instead of the vector extending.
    /// Probed against the vendor (dense scans over the integer operand):
    ///
    /// * ordered (`<` family): the integer wraps mod 2ⁿ — `conv_unsigned`
    ///   for an `unsigned` vector (unsigned compare), `conv_signed` for a
    ///   `signed` one (`unsigned(1111) < 16` is FALSE: 16 wraps to 0).
    /// * equality with a `signed` vector: wraps mod 2ⁿ signed
    ///   (`signed(1111) = 15` is TRUE: 15 wraps to -1).
    /// * equality with an `unsigned` vector: the integer wraps mod 2ⁿ⁺¹
    ///   as a signed (N+1)-bit value against the zero-padded vector — the
    ///   width needed to hold the unsigned range as signed
    ///   (`unsigned(1111) = 47` is TRUE, `= 31` is FALSE).
    ///
    /// Same-type vector comparisons keep the extend-to-wider behavior —
    /// the vendor agrees with hdlsim there (probed: `uns8(16) = uns4(15)`
    /// is false on both). Extending instead of wrapping would break
    /// designs that compare a log2-wide counter against `G_SIZE - 1`.
    /// Objects whose class is not confidently known (plain vectors,
    /// records, dynamic constants) skip the rewrite and keep the
    /// extend-to-wider comparison.
    fn wrap_mixed_compare(&self, op: BinOp, l: Expr, r: Expr) -> (Expr, Expr) {
        if !matches!(
            op,
            BinOp::Eq
                | BinOp::EqStrict
                | BinOp::Ne
                | BinOp::NeStrict
                | BinOp::Lt
                | BinOp::Le
                | BinOp::Gt
                | BinOp::Ge
        ) {
            return (l, r);
        }
        // The wrap is std_logic_arith's overload behavior; numeric_std's
        // mixed comparisons extend instead. The package participates via
        // this file's use clauses or the inherited entity context.
        if !self.arith_visible {
            return (l, r);
        }
        match (self.expr_obj_class(&l), self.expr_obj_class(&r)) {
            (CmpOperand::Arith { signed, width }, CmpOperand::Integer) => {
                self.wrap_mixed_sides(op, l, r, signed, width)
            }
            (CmpOperand::Integer, CmpOperand::Arith { signed, width }) => {
                let (vw, iw) = self.wrap_mixed_sides(op, r, l, signed, width);
                (iw, vw)
            }
            _ => (l, r),
        }
    }

    /// Wrap one mixed comparison whose vector side is `v` (classified
    /// `signed`/`width`) and integer side is `i`; returns (vector, integer)
    /// in the caller's order. See [`Self::wrap_mixed_compare`] for the
    /// vendor rules.
    fn wrap_mixed_sides(
        &self,
        op: BinOp,
        v: Expr,
        i: Expr,
        signed: bool,
        width: usize,
    ) -> (Expr, Expr) {
        if matches!(
            op,
            BinOp::Eq | BinOp::EqStrict | BinOp::Ne | BinOp::NeStrict
        ) && !signed
        {
            // Unsigned equality: zero-pad the vector and wrap the integer
            // as a signed value in N+1 bits, then compare bitwise.
            return (
                Expr::Resize {
                    e: Box::new(v),
                    width: width + 1,
                    signed: false,
                },
                Expr::Resize {
                    e: Box::new(i),
                    width: width + 1,
                    signed: true,
                },
            );
        }
        // Ordered (both vector kinds) and signed equality: the integer
        // wraps to the vector's width with the vector's signedness
        // (conv_unsigned / conv_signed).
        (
            v,
            Expr::Resize {
                e: Box::new(i),
                width,
                signed,
            },
        )
    }

    /// Classify a lowered expression for [`Self::wrap_mixed_compare`]:
    /// `unsigned`/`signed` objects and `signed(x)`/`conv_*` conversions are
    /// arith vectors (the wrap target, with the width to wrap to); integer
    /// literals, integer objects and integer arithmetic are the wrapped
    /// side; everything else stays untouched. Best effort — unknown shapes
    /// classify as [`CmpOperand::Other`] and keep the current comparison.
    fn expr_obj_class(&self, e: &Expr) -> CmpOperand {
        match e {
            // Only plain decimal literals are VHDL integers (32-bit —
            // `parse_int_literal`'s convention). Any other `Lit` shape is a
            // vector/char/time literal (bit-string literals adopt the
            // vector's subtype in VHDL-2008) and compares element-wise, so
            // it keeps the same-type comparison. A 32-bit sized bit-string
            // literal in an ordered mixed comparison is misread as integer
            // (wraps instead of extends) — accepted, it is the only shape
            // the width cannot separate.
            Expr::Lit(v) if v.width() == 32 => CmpOperand::Integer,
            Expr::Sig(i) => self.sig_obj_class(*i),
            Expr::Signed(inner) => self.signed_expr_class(inner),
            // `conv_signed`/`conv_unsigned`/`resize` results: arith vectors
            // with the conversion width.
            Expr::Resize { width, signed, .. } => CmpOperand::Arith {
                signed: *signed,
                width: *width,
            },
            // Integer arithmetic over integer operands stays integer
            // (`G_SEGMENT_SIZE - 1`); anything involving a non-integer side
            // is too shape-dependent to classify confidently.
            Expr::Bin { l, r, .. } => match (self.expr_obj_class(l), self.expr_obj_class(r)) {
                (CmpOperand::Integer, CmpOperand::Integer) => CmpOperand::Integer,
                _ => CmpOperand::Other,
            },
            Expr::Un { e, .. } => match self.expr_obj_class(e) {
                CmpOperand::Integer => CmpOperand::Integer,
                _ => CmpOperand::Other,
            },
            _ => CmpOperand::Other,
        }
    }

    /// Classify a signal/slot operand from its declared object class
    /// (recorded by `record_obj_shape` from the declaration's type mark).
    fn sig_obj_class(&self, id: SigId) -> CmpOperand {
        match self.sig_class.get(&id) {
            Some(ObjClass::Arith) => CmpOperand::Arith {
                signed: self.sig_signed.get(&id).copied().unwrap_or(false),
                width: self.sig_width.get(&id).copied().unwrap_or(1).max(1),
            },
            Some(ObjClass::Integer) => CmpOperand::Integer,
            _ => CmpOperand::Other,
        }
    }

    /// Classify a `signed(x)` conversion result: it keeps the operand's
    /// bits but marks signed semantics, so the result is a signed arith
    /// vector regardless of the operand's declared class. A `Signed`-wrapped
    /// LITERAL is an integer fold (signed-constant use, unary minus), not a
    /// vector.
    fn signed_expr_class(&self, inner: &Expr) -> CmpOperand {
        match inner {
            // A `Signed`-wrapped LITERAL is an integer fold (signed-constant
            // use, unary minus) only for the 32-bit plain-decimal shape; a
            // narrower literal is a bit pattern (`signed(x"FF")`) and keeps
            // the element-wise comparison.
            Expr::Lit(v) if v.width() == 32 => CmpOperand::Integer,
            _ => match self.expr_obj_class(inner) {
                CmpOperand::Arith { width, .. } => CmpOperand::Arith {
                    signed: true,
                    width,
                },
                CmpOperand::Integer => CmpOperand::Integer,
                // `signed(slv_sig)`: the conversion makes it a signed arith
                // vector; only signal operands carry a confident width
                // (estimates would mis-size the wrap).
                CmpOperand::Other => match inner {
                    Expr::Sig(s) => CmpOperand::Arith {
                        signed: true,
                        width: self.sig_width.get(s).copied().unwrap_or(1).max(1),
                    },
                    _ => CmpOperand::Other,
                },
            },
        }
    }

    /// A 32-bit vector literal is wrapped in an identity `Resize` so the
    /// comparison classifier sees an arith vector, not a plain-decimal
    /// integer: `x"80000000"` and `2147483648` lower to the same 32-bit
    /// `Lit`, but the bit-string form adopts the compared vector's subtype
    /// (VHDL-2008) and must keep the same-type comparison. Evaluation is
    /// unchanged — a `Resize` to the value's own width is identity.
    pub(super) fn lit_vec_mark(&self, v: Value) -> Expr {
        if v.width() == 32 {
            let width = v.width();
            Expr::Resize {
                e: Box::new(Expr::Lit(v)),
                width,
                signed: false,
            }
        } else {
            Expr::Lit(v)
        }
    }
}
