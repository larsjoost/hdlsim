//! Object naming: signal/object id resolution, record-field offsets and
//! real-typed ranges, hidden-constant materialization, and the tiny
//! const-eval wrappers. Selected-name chains live in `chains`.
//!
//! Extracted verbatim from vhdl_lower.rs (pure move, no logic changes).

use super::*;

impl<'a> Lower<'a> {
    /// Hidden signal for an array-typed constant: a memory of per-element
    /// width holding the flat value.
    fn materialize_const_array(
        &mut self,
        module: &mut ModuleIr,
        hidden: &str,
        val: Value,
        ti: TypeInfo,
    ) {
        let TypeInfo::Array {
            elem,
            depth,
            dims,
            elem_name,
        } = ti
        else {
            return;
        };
        // An unconstrained array type carries its declaration
        // depth (1 for `array (natural range <>)`); the folded
        // VALUE knows the real element count — derive the depth
        // from the value so `c(i)` for every folded element
        // resolves (a 1-word memory X-fills the rest).
        let elem = elem.max(1);
        let depth = if dims.is_empty() && depth <= 1 {
            (val.width() / elem).max(1)
        } else {
            depth.max(1)
        };
        let id = module.signals.len() as SigId;
        module.signals.push(SignalIr {
            name: hidden.to_string(),
            width: elem,
            kind: SigKind::Wire,
            init: Some(val),
            mem: Some(depth),
            signed: false,
        });
        self.names.insert(hidden.to_string(), id);
        self.mems.insert(id, depth);
        self.sig_width.insert(id, elem);
        if !dims.is_empty() {
            self.sig_dims.insert(id, dims);
        }
        if let Some(en) = elem_name {
            if matches!(self.types.get(&en), Some(TypeInfo::Record { .. })) {
                self.sig_elem_rec.insert(id, en);
            }
        }
    }

    /// Materialize a record/array-typed constant as a hidden signal holding
    /// its value, so runtime selection works: `c(i)`, `c(i).field`, `c.field`
    /// on a constant lower to Idx/Sel over `__const_<name>`.
    pub(super) fn materialize_const(&mut self, module: &mut ModuleIr, d: &SyntaxNode) {
        let kids = real_children(d);
        let Some(colon) = kids
            .iter()
            .position(|k| k.kind() == SyntaxKind::Punct && self.text(k) == ":")
        else {
            return;
        };
        let names: Vec<String> = kids[..colon]
            .iter()
            .filter(|k| k.kind() == SyntaxKind::Name)
            .map(|k| self.text(k).to_ascii_lowercase())
            .collect();
        for n in names {
            let hidden = format!("__const_{}", n);
            if self.names.contains_key(&hidden) {
                continue;
            }
            let Some(tn) = self.const_types.get(&n).cloned() else {
                continue;
            };
            let Some(ti) = self.types.get(&tn).cloned() else {
                continue;
            };
            let Some(val) = self.consts.get(&n).cloned() else {
                continue;
            };
            match ti {
                ti @ TypeInfo::Array { .. } => {
                    self.materialize_const_array(module, &hidden, val, ti);
                }
                TypeInfo::Record { .. } => {
                    self.materialize_const_record(module, &hidden, val, &tn);
                }
                _ => {}
            }
        }
    }

    /// Hidden signal for a record-typed constant: width is the summed
    /// field width, the record type is remembered for field selects, and
    /// the real-typed leaf ranges are registered for wave display.
    fn materialize_const_record(
        &mut self,
        module: &mut ModuleIr,
        hidden: &str,
        val: Value,
        tn: &str,
    ) {
        let Some(TypeInfo::Record { fields }) = self.types.get(tn) else {
            return;
        };
        let width = fields.iter().map(|f| f.1).sum::<usize>().max(1);
        let id = module.signals.len() as SigId;
        module.signals.push(SignalIr {
            name: hidden.to_string(),
            width,
            kind: SigKind::Wire,
            init: Some(val),
            mem: None,
            signed: false,
        });
        self.names.insert(hidden.to_string(), id);
        self.sig_width.insert(id, width);
        self.sig_rec.insert(id, tn.to_string());
        module
            .real_ranges
            .insert(hidden.to_string(), self.record_real_ranges(tn));
    }

    /// `c(i)` / `c(i, j)` where `c` is an array- or vector-typed constant
    /// (e.g. a function-local constant, which cannot be a module signal
    /// because function bodies lower in slot space): lower to a `$const_idx`
    /// system call over the literal value — args are [flat value, flat
    /// index, element width]. The flat index is MSB-first (element 0 = most
    /// significant element), the one mapping both the const-eval and the
    /// runtime evaluate; a materialized hidden signal is invisible to the
    /// const-eval, so constant initializers must take this route.
    pub(super) fn lower_const_array_index(&mut self, fname: &str, args: &[Expr]) -> Option<Expr> {
        let tn = self.const_types.get(fname)?.clone();
        let val = self.consts.get(fname)?.clone();
        let ew = self.const_elem_width(&tn)?;
        let flat = self.flat_const_index(fname, &tn, args, val.width(), ew)?;
        // An element of an array-of-integers constant IS an integer: mark it
        // so the runtime overload picker can type-match the actual
        // (`arg_hint` reads the `Signed` marker, bit-identical in eval).
        // Without it, same-width overloads from other packages — an
        // unconstrained `std_ulogic_vector` formal also resolves to 32 —
        // absorb the call by registration order and `to_string(int_elem)`
        // binds the bit-rendering overload.
        let int_elem = match self.types.get(&tn) {
            Some(TypeInfo::Array {
                elem_name: Some(en),
                ..
            }) => matches!(self.types.get(en), Some(TypeInfo::Integer { .. })),
            _ => false,
        };
        let elem = Expr::Call {
            name: "$const_idx".into(),
            args: vec![
                Expr::Lit(val),
                flat,
                Expr::Lit(Value::from_u64(32, ew as u64)),
            ],
            shapes: Vec::new(),
        };
        Some(if int_elem {
            Expr::Signed(Box::new(elem))
        } else {
            elem
        })
    }

    /// Element width in bits of constant `fname`'s array-like type:
    /// `TypeInfo::Array` carries it, the vector type marks resolve through
    /// `array_elem_width` (1-bit elements unless `string`).
    pub(super) fn const_elem_width(&self, tn: &str) -> Option<usize> {
        match self.types.get(tn)? {
            TypeInfo::Array { elem, .. } => Some(*elem),
            TypeInfo::Vector { .. } => Some(self.array_elem_width(tn).unwrap_or(1)),
            _ => None,
        }
    }

    /// Per-dim `(low, size, ascending)` of constant `fname`: the object
    /// constraint's shape (`obj_dims`/`obj_asc`) when recorded, else the
    /// type's dims (flat descending default), else the value's single dim.
    fn const_dims_meta(
        &self,
        fname: &str,
        tn: &str,
        bits: usize,
        ew: usize,
    ) -> Vec<(i64, usize, bool)> {
        let dirs = self.obj_asc.get(fname).cloned();
        let dims = self
            .obj_dims
            .get(fname)
            .cloned()
            .or_else(|| match self.types.get(tn) {
                Some(TypeInfo::Array { dims, .. }) => Some(dims.clone()),
                _ => None,
            })
            .unwrap_or_else(|| {
                // Unrecorded shape: the element count comes from the packed
                // width; a `string` (8-bit elements) defaults to the LRM
                // `1 TO n` range, everything else to the flat 0-based
                // mapping.
                let lo = if ew == 8 { 1 } else { 0 };
                vec![(lo, (bits / ew.max(1)).max(1))]
            });
        dims.into_iter()
            .enumerate()
            .map(|(d, (lo, n))| {
                // Unrecorded directions keep the flat direct mapping (the
                // pre-dims behavior): only a recorded descending constraint
                // flips the index.
                let asc = dirs
                    .as_ref()
                    .and_then(|v| v.get(d))
                    .copied()
                    .unwrap_or(true);
                (lo, n, asc)
            })
            .collect()
    }

    /// Flat MSB-first index over the constant's dims for the given index
    /// args. An ascending dim addresses directly from its low bound; a
    /// descending dim's flat position flips against its size. A shape/arg
    /// mismatch falls back to the first arg (the pre-dims behavior).
    fn flat_const_index(
        &self,
        fname: &str,
        tn: &str,
        args: &[Expr],
        bits: usize,
        ew: usize,
    ) -> Option<Expr> {
        let dims = self.const_dims_meta(fname, tn, bits, ew);
        if dims.is_empty() || dims.len() != args.len() {
            return args.first().cloned();
        }
        let mut flat: Option<Expr> = None;
        for (arg, (lo, n, asc)) in args.iter().zip(dims.iter()) {
            let off = Expr::Bin {
                op: BinOp::Sub,
                l: Box::new(arg.clone()),
                r: Box::new(Expr::Lit(Value::from_u64(32, (*lo).max(0) as u64))),
            };
            let e = if *asc {
                off
            } else {
                Expr::Bin {
                    op: BinOp::Sub,
                    l: Box::new(Expr::Lit(Value::from_u64(
                        32,
                        (*n as i64 - 1).max(0) as u64,
                    ))),
                    r: Box::new(off),
                }
            };
            flat = Some(match flat {
                None => e,
                Some(f) => Expr::Bin {
                    op: BinOp::Add,
                    l: Box::new(Expr::Bin {
                        op: BinOp::Mul,
                        l: Box::new(f),
                        r: Box::new(Expr::Lit(Value::from_u64(32, *n as u64))),
                    }),
                    r: Box::new(e),
                },
            });
        }
        flat
    }

    pub(super) fn sig_id(&self, name: &str) -> Option<SigId> {
        // Innermost scope first: the current map, then enclosing scopes
        // (blocks, subprograms) innermost-out — VHDL visibility (LRM 10.2).
        // VHDL is case-insensitive: fall back to a case-insensitive scan
        // (`delay` must find a parameter declared as `Delay`).
        if let Some(id) = self.names.get(name).copied() {
            return Some(id);
        }
        if let Some((_, id)) = self
            .names
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
        {
            return Some(*id);
        }
        for scope in self.names_stack.iter().rev() {
            if let Some(id) = scope.get(name).copied() {
                return Some(id);
            }
            if let Some((_, id)) = scope.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)) {
                return Some(*id);
            }
        }
        None
    }

    /// Resolve a name to a flat signal id, falling back to the hidden signal
    /// a materialized constant lowers to (`c_arr` → `__const_c_arr`): array
    /// attributes on constants (`c'range`, `c'length`) index that signal.
    pub(super) fn object_id(&self, name: &str) -> Option<SigId> {
        self.sig_id(name).or_else(|| {
            self.names
                .get(&format!("__const_{}", name.to_ascii_lowercase()))
                .copied()
        })
    }

    /// Xilinx implicit globals (`GSR`/`GRESTORE`/`GTS` from the auto-instantiated
    /// `glbl` primitive, referenced bare in unisim VHDL): materialize a
    /// per-module 1-bit signal on first reference. Caller must have armed
    /// `extra_base` (== `module.signals.len()`) for the current phase.
    pub(super) fn ensure_global_sig(&mut self, name: &str) -> SigId {
        if let Some(id) = self.sig_id(name) {
            return id;
        }
        let id = (self.extra_base + self.extra_signals.len()) as SigId;
        self.extra_signals.push(SignalIr {
            name: name.to_string(),
            width: 1,
            kind: SigKind::Wire,
            init: None,
            mem: None,
            signed: false,
        });
        self.sig_width.insert(id, 1);
        self.sig_signed.insert(id, false);
        self.names.insert(name.to_string(), id);
        id
    }

    /// If `tn` is (or is an access to) a record type, return the record type
    /// name — access-typed signals get implicit `.all` on field selects.
    pub(super) fn as_record(&self, tn: &str) -> Option<String> {
        // `X.field'subtype` where X is a record-typed signal: the field's type.
        if let Some((base, rest)) = tn.split_once('.') {
            let path: Vec<&str> = rest.split('.').collect();
            if let Some(ft) = self.record_type_at_path(base, &path) {
                if matches!(self.types.get(&ft), Some(TypeInfo::Record { .. })) {
                    return Some(ft);
                }
            }
        }
        // Selected type mark (`work.p.rec_t`): the bare item name is registered.
        let tn = tn.rsplit('.').next().unwrap_or(tn);
        if matches!(self.types.get(tn), Some(TypeInfo::Record { .. })) {
            return Some(tn.to_string());
        }
        if let Some(inner) = self.access_inner.get(tn) {
            if matches!(self.types.get(inner), Some(TypeInfo::Record { .. })) {
                return Some(inner.clone());
            }
        }
        // `X'subtype` where X is a record-typed port/signal: adopt its record.
        if let Some(id) = self.sig_id(tn) {
            if let Some(rn) = self.sig_rec.get(&id) {
                return Some(rn.clone());
            }
        }
        None
    }

    /// If `tn` is (or is an access to) an array-of-records, return the element
    /// record type name (for `arr(i).field`).
    pub(super) fn as_elem_record(&self, tn: &str) -> Option<String> {
        // Selected type mark (`work.p.arr_t`): the bare item name is registered.
        let tn = tn.rsplit('.').next().unwrap_or(tn);
        let get = |t: &str| match self.types.get(t) {
            Some(TypeInfo::Array {
                elem_name: Some(en),
                ..
            }) => matches!(self.types.get(en), Some(TypeInfo::Record { .. })).then(|| en.clone()),
            _ => None,
        };
        get(tn).or_else(|| self.access_inner.get(tn).and_then(|i| get(i)))
    }

    /// Record type at the end of a field path rooted at a record-typed signal
    /// (`sig.a.b` → the type of field `b`).
    pub(super) fn record_type_at_path(&self, base: &str, path: &[&str]) -> Option<String> {
        let mut cur = match self.sig_id(base) {
            Some(id) => self.sig_rec.get(&id)?.clone(),
            // Record alias base (`alias LAW is AxiBus.WriteAddress;`).
            None => self.alias_rec.get(base)?.clone(),
        };
        for seg in path {
            cur = self
                .rec_field_ty
                .get(&(cur.clone(), seg.to_ascii_lowercase()))?
                .clone();
        }
        Some(cur)
    }

    /// Offset (lsb, width) of a (possibly nested) record field path inside
    /// record type `tn`; fields are packed first-declared = MSB.
    pub(super) fn record_field_offset(&self, tn: &str, segs: &[&str]) -> Option<(usize, usize)> {
        let mut cur = tn.to_string();
        let mut base_lsb = 0usize;
        for (si, seg) in segs.iter().enumerate() {
            let TypeInfo::Record { fields } = self.types.get(&cur)? else {
                return None;
            };
            // Protected type (registered as an empty record): any member
            // select is an opaque placeholder.
            if fields.is_empty() {
                return Some((0, 32));
            }
            let total: usize = fields.iter().map(|f| f.1).sum();
            let mut run = 0usize;
            let mut found: Option<(usize, usize, Option<String>)> = None;
            for (fn_, w, _) in fields {
                let lsb = total - run - w;
                if fn_ == seg {
                    found = Some((
                        lsb,
                        *w,
                        self.rec_field_ty.get(&(cur.clone(), fn_.clone())).cloned(),
                    ));
                    break;
                }
                run += *w;
            }
            let (lsb, w, fty) = found?;
            base_lsb += lsb;
            if si + 1 < segs.len() {
                cur = fty?;
            } else {
                return Some((base_lsb, w));
            }
        }
        None
    }

    /// Inclusive bit ranges of real-typed leaves within record type `tn`
    /// (first field = MSB, like [`Self::record_fields_of_type`]). Nested
    /// record fields recurse with the outer offset. The simulator decodes
    /// those slices as IEEE-754 reals; everything else as a bit pattern.
    pub(super) fn record_real_ranges(&self, tn: &str) -> Vec<(u32, u32)> {
        let mut out = Vec::new();
        self.collect_real_ranges(tn, 0, &mut out);
        out
    }

    pub(super) fn collect_real_ranges(&self, tn: &str, base: usize, out: &mut Vec<(u32, u32)>) {
        let Some(TypeInfo::Record { fields }) = self.types.get(tn) else {
            return;
        };
        let total: usize = fields.iter().map(|f| f.1).sum();
        let mut run = 0usize;
        for (fn_, w, _) in fields {
            let lsb = total - run - w;
            let fty = self
                .rec_field_ty
                .get(&(tn.to_string(), fn_.clone()))
                .cloned();
            match fty.as_deref().map(|t| self.types.get(t)) {
                Some(Some(TypeInfo::Real)) => {
                    out.push(((base + lsb) as u32, (base + lsb + w - 1) as u32));
                }
                Some(Some(TypeInfo::Record { .. })) => {
                    self.collect_real_ranges(fty.as_deref().unwrap_or(""), base + lsb, out);
                }
                _ => {}
            }
            run += w;
        }
    }

    /// Const-eval in the lowerer's constant scope, with visible package
    /// function bodies (for calls in constant initializers).
    pub(super) fn ce(&self, e: &Expr) -> Option<Value> {
        const_eval_f_with_pkgs(e, &self.consts, &self.funcs, &self.funcs_ce)
    }

    /// Const-eval against an explicit env (loop vars, generics) plus package
    /// function bodies.
    pub(super) fn ce_env(&self, e: &Expr, env: &HashMap<String, Value>) -> Option<Value> {
        const_eval_f_with_pkgs(e, env, &self.funcs, &self.funcs_ce)
    }

    // ---- entities / architectures ---------------------------------------------
}
