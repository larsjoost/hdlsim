//! The fname dispatch tail of VHDL call lowering: after the sequential
//! early-return routes in `super::call`, the call name selects between
//! signal indexing, shift/rotate and vendor-shift folds, edge and type
//! conversions, resizes, operator-symbol calls, subprogram aliases, and
//! the plain runtime call. Family helpers mirror the original match-arm
//! order; each returns `None` when its name/arity guard does not hold,
//! reproducing the match's fall-through.

use super::*;

impl<'a> Lower<'a> {
    /// The fname dispatch, in the original match-arm order: the signal-
    /// index guard first (it is a catch-all guard, so it must win over
    /// the fname-specific arms), then the shift/conversion/operator/
    /// alias families, and finally the plain call. Each family helper
    /// returns `None` when its name/arity guard does not hold, which
    /// reproduces the match's fall-through to the next arm.
    pub(super) fn dispatch_call(
        &mut self,
        base_id: Option<SigId>,
        fname: String,
        args: Vec<Expr>,
        arg_nodes: &[SyntaxNode],
        env: &HashMap<String, Value>,
    ) -> Expr {
        // Indexed write/read of a signal: v(i) → Idx; multi-dimensional
        // v(i, j, …) → flat index ((i-lo0)*n1 + (j-lo1))*… via sig_dims.
        // A name that is also a declared function stays a call in call
        // position: inside the function's own body the name resolves to
        // the result slot (slot 0), which must not turn a recursive call
        // into an index of that slot.
        if let Some(base) = base_id {
            if !args.is_empty() && !self.funcs.contains_key(&fname) {
                return self.signal_index_call(base, &fname, &args);
            }
        }
        if let Some(ex) = self.shift_call(&fname, &args) {
            return ex;
        }
        if let Some(ex) = self.vendor_shift_call(&fname, &args) {
            return ex;
        }
        if let Some(ex) = self.edge_call(&fname, &args) {
            return ex;
        }
        if let Some(ex) = self.int_convert_call(&fname, &args) {
            return ex;
        }
        if let Some(ex) = self.real_convert_call(&fname, &args) {
            return ex;
        }
        if let Some(ex) = self.resize_call(&fname, &args, env) {
            return ex;
        }
        if let Some(ex) = self.resize_signed_call(&fname, &args, env) {
            return ex;
        }
        if let Some(ex) = self.type_convert_call(&fname, &args) {
            return ex;
        }
        if let Some(ex) = self.operator_call(&fname, &args) {
            return ex;
        }
        if let Some(ex) = self.alias_call(&fname, &args, arg_nodes) {
            return ex;
        }
        self.plain_call(fname, args, arg_nodes)
    }

    /// Signal indexing under the dispatch's first guard: multi-
    /// dimensional flat indexing via `sig_dims`, then single-index
    /// element select, then the plain call fallback.
    fn signal_index_call(&mut self, base: SigId, fname: &str, args: &[Expr]) -> Expr {
        if let Some(ex) = self.flat_index_call(base, args) {
            return ex;
        }
        if args.len() == 1 {
            let k = &args[0];
            if !self.mems.contains_key(&base) {
                if let Some(se) = self.elem_sel(base, k) {
                    return se;
                }
            }
            return Expr::Idx {
                e: Box::new(Expr::Sig(base)),
                i: Box::new(k.clone()),
            };
        }
        Expr::Call {
            name: fname.to_string(),
            args: args.to_vec(),
            shapes: Vec::new(),
        }
    }

    /// Multi-dimensional index fold: ((i-lo0)*n1 + (j-lo1))*… over the
    /// recorded per-dimension (low, size) pairs.
    fn flat_index_call(&self, base: SigId, args: &[Expr]) -> Option<Expr> {
        let dims = self.sig_dims.get(&base).cloned()?;
        if dims.len() != args.len() {
            return None;
        }
        let mut flat: Option<Expr> = None;
        for (arg, (lo, n)) in args.iter().zip(dims.iter()) {
            let a = Expr::Bin {
                op: BinOp::Sub,
                l: Box::new(arg.clone()),
                r: Box::new(Expr::Lit(Value::from_u64(32, *lo as u64))),
            };
            flat = Some(match flat {
                None => a,
                Some(f) => Expr::Bin {
                    op: BinOp::Add,
                    l: Box::new(Expr::Bin {
                        op: BinOp::Mul,
                        l: Box::new(f),
                        r: Box::new(Expr::Lit(Value::from_u64(32, *n as u64))),
                    }),
                    r: Box::new(a),
                },
            });
        }
        Some(Expr::Idx {
            e: Box::new(Expr::Sig(base)),
            i: Box::new(flat.expect(
                "dims.len() == args.len() and args is non-empty, so the fold assigned flat",
            )),
        })
    }

    /// Shift/rotate function forms from std_logic_arith & co.
    fn shift_call(&self, fname: &str, args: &[Expr]) -> Option<Expr> {
        let op = match fname {
            "shift_left" | "sll" | "sla" if args.len() == 2 => BinOp::Shl,
            "shift_right" | "srl" if args.len() == 2 => BinOp::Shr,
            "sra" if args.len() == 2 => BinOp::Sar,
            "rotate_left" | "rol" if args.len() == 2 => BinOp::Rol,
            "rotate_right" | "ror" if args.len() == 2 => BinOp::Ror,
            _ => return None,
        };
        Some(Expr::Bin {
            op,
            l: Box::new(args[0].clone()),
            r: Box::new(args[1].clone()),
        })
    }

    /// Vendor-package vector shifts: `SHR(vec, n)` / `SHL(vec, n)`
    /// from the std_logic_arith idiom play the same role as
    /// shift_right/shift_left — fold them to plain shifts so they
    /// never reach runtime dispatch. Delegating overload bodies
    /// (`RETURN SHR(arg, conv_unsigned(count, arg'length))`) have no
    /// type-correct dispatch target — the visible overloads are the
    /// delegators themselves — and a folded shift terminates by
    /// construction. A record-typed first actual
    /// (shr(float_t, natural) families) must stay a real call: the
    /// record's flattened width can coincide with a vector's, and a
    /// folded shift would mangle the record's fields.
    fn vendor_shift_call(&self, fname: &str, args: &[Expr]) -> Option<Expr> {
        if !matches!(fname, "shr" | "shl") || args.len() != 2 {
            return None;
        }
        let record = match &args[0] {
            Expr::Sig(s) => self.sig_rec.contains_key(s),
            _ => false,
        };
        if record {
            return Some(Expr::Call {
                name: fname.to_string(),
                args: args.to_vec(),
                shapes: Vec::new(),
            });
        }
        Some(Expr::Bin {
            op: if fname == "shl" {
                BinOp::Shl
            } else {
                BinOp::Shr
            },
            l: Box::new(args[0].clone()),
            r: Box::new(args[1].clone()),
        })
    }

    /// Edge-detection function forms.
    fn edge_call(&self, fname: &str, args: &[Expr]) -> Option<Expr> {
        match (fname, args.len()) {
            ("rising_edge", 1) => Some(Expr::Rising(Box::new(args[0].clone()))),
            ("falling_edge", 1) => Some(Expr::Falling(Box::new(args[0].clone()))),
            _ => None,
        }
    }

    /// Integer type conversions.
    fn int_convert_call(&self, fname: &str, args: &[Expr]) -> Option<Expr> {
        if !matches!(
            fname,
            "to_integer" | "conv_integer" | "integer" | "natural" | "positive"
        ) || args.len() != 1
        {
            return None;
        }
        Some(Expr::ToInt(Box::new(args[0].clone())))
    }

    /// Real/fixed/float conversions: `x * 1.0` (real-typed f64).
    /// `real(x)` is a value conversion (LRM 8.6) — integer bits do
    /// NOT pass through unchanged; a bit-identical passthrough made
    /// `RETURN conv_float(real(arg))` call itself with the same
    /// integer and recurse forever.
    fn real_convert_call(&self, fname: &str, args: &[Expr]) -> Option<Expr> {
        match (fname, args.len()) {
            ("to_real" | "real" | "to_ufixed" | "to_sfixed" | "to_float", n) if n > 0 => {
                Some(as_real(args[0].clone()))
            }
            ("resize", 3) => Some(as_real(args[0].clone())),
            _ => None,
        }
    }

    /// Vector resize/convert forms with an explicit target width.
    fn resize_call(
        &self,
        fname: &str,
        args: &[Expr],
        env: &HashMap<String, Value>,
    ) -> Option<Expr> {
        // `to_slv(x)`: bit-identical view as std_logic_vector.
        if fname == "to_slv" && args.len() == 1 {
            return Some(args[0].clone());
        }
        let width = |args: &[Expr]| {
            self.ce_env(&args[1], env)
                .and_then(|w| w.to_u64())
                .unwrap_or(32) as usize
        };
        if matches!(
            fname,
            "to_unsigned" | "conv_unsigned" | "conv_std_logic_vector"
        ) && args.len() == 2
        {
            return Some(Expr::Resize {
                e: Box::new(args[0].clone()),
                width: width(args),
                signed: false,
            });
        }
        if matches!(fname, "to_signed" | "conv_signed") && args.len() == 2 {
            return Some(Expr::Resize {
                e: Box::new(args[0].clone()),
                width: width(args),
                signed: true,
            });
        }
        None
    }

    /// `resize(x, w)` — signedness inherited from the operand — and
    /// `signed(x)`, which keeps the bits but marks signed semantics.
    fn resize_signed_call(
        &self,
        fname: &str,
        args: &[Expr],
        env: &HashMap<String, Value>,
    ) -> Option<Expr> {
        if fname == "resize" && args.len() == 2 {
            let w = self
                .ce_env(&args[1], env)
                .and_then(|w| w.to_u64())
                .unwrap_or(32) as usize;
            let signed = match &args[0] {
                Expr::Sig(s) => self.sig_signed.get(s).copied().unwrap_or(false),
                Expr::Signed(_) => true,
                Expr::Resize { signed, .. } => *signed,
                _ => false,
            };
            return Some(Expr::Resize {
                e: Box::new(args[0].clone()),
                width: w,
                signed,
            });
        }
        // `signed(x)` keeps the bits but marks signed semantics.
        if fname == "signed" && args.len() == 1 {
            return Some(Expr::Signed(Box::new(args[0].clone())));
        }
        None
    }

    /// Other type conversions are bit-identical.
    fn type_convert_call(&self, fname: &str, args: &[Expr]) -> Option<Expr> {
        if matches!(
            fname,
            "std_logic_vector"
                | "std_ulogic_vector"
                | "unsigned"
                | "bit_vector"
                | "std_logic"
                | "bit"
                | "boolean"
                | "string"
                | "char"
        ) && args.len() == 1
        {
            return Some(args[0].clone());
        }
        None
    }

    /// Operator-symbol call: `"<"(a, b)`, `"+"(x, y)`.
    fn operator_call(&self, fname: &str, args: &[Expr]) -> Option<Expr> {
        let op = vhdl_binop(fname.trim_matches('"'))?;
        if args.len() != 2 {
            return None;
        }
        Some(self.mark_signed_compare(op, args[0].clone(), args[1].clone()))
    }

    /// Alias to a subprogram: `alias a is f[...];` — calls to `a(...)`
    /// become calls to the target `f(...)`.
    fn alias_call(&mut self, fname: &str, args: &[Expr], arg_nodes: &[SyntaxNode]) -> Option<Expr> {
        if !self
            .aliases
            .get(fname)
            .is_some_and(|a| matches!(a, Expr::Call { .. }))
        {
            return None;
        }
        let target = match self
            .aliases
            .get(fname)
            .expect("arm guard checked that this alias is a Call")
        {
            Expr::Call { name: target, .. } => target.clone(),
            _ => unreachable!(),
        };
        let shapes = self.call_shape_binds(&target, args, arg_nodes);
        Some(Expr::Call {
            name: target,
            args: args.to_vec(),
            shapes,
        })
    }

    /// Every other name: a runtime call with the resolved shape binds.
    fn plain_call(&mut self, fname: String, args: Vec<Expr>, arg_nodes: &[SyntaxNode]) -> Expr {
        let shapes = self.call_shape_binds(&fname, &args, arg_nodes);
        Expr::Call {
            name: fname,
            args,
            shapes,
        }
    }
}
