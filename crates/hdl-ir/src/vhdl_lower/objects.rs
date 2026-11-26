//! Object naming and selected-name chains: signal/object id resolution, record-field offsets, ChainSeg chain flattening, hidden-constant materialization, and the tiny const-eval wrappers.
//!
//! Extracted verbatim from vhdl_lower.rs (pure move, no logic changes).

use super::*;

/// A selected-name chain's resolved root: either a terminal literal — a
/// constant root folding from its VALUE (constant initializers like
/// `constant x : t := c.field;` cannot read the materialized hidden
/// signal, and the value is equally valid at runtime: field selects and
/// `$const_idx` evaluate over literals) — or a walk start.
enum ChainRoot {
    Lit(Value),
    Walk {
        cur: Expr,
        cur_ty: String,
        /// Element-record type of an array-of-records root, from the slot
        /// registration; consumed by the first Index segment.
        root_elem: Option<String>,
    },
}

/// Linearize a multi-dimensional index group against per-dimension
/// `(low, size)` bounds: `(i-lo0)*n1*n2 + (j-lo1)*n2 + (k-lo2)`. Same
/// formula as `lower_call`'s signal-array path and
/// `lower_const_array_index`.
pub(super) fn flatten_dims(
    args: &[SyntaxNode],
    dims: &[(i64, usize)],
    env: &HashMap<String, Value>,
    lx: &mut Lower,
) -> Expr {
    let mut flat: Option<Expr> = None;
    for (arg, (lo, n)) in args.iter().zip(dims.iter()) {
        let a = Expr::Bin {
            op: BinOp::Sub,
            l: Box::new(lx.lower_expr(arg, env)),
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
    flat.unwrap_or_else(|| Expr::Lit(Value::from_u64(32, 0)))
}

impl<'a> Lower<'a> {
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
                TypeInfo::Array {
                    elem,
                    depth,
                    dims,
                    elem_name,
                } => {
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
                        name: hidden.clone(),
                        width: elem,
                        kind: SigKind::Wire,
                        init: Some(val),
                        mem: Some(depth),
                        signed: false,
                    });
                    self.names.insert(hidden, id);
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
                TypeInfo::Record { fields } => {
                    let width = fields.iter().map(|f| f.1).sum::<usize>().max(1);
                    let id = module.signals.len() as SigId;
                    module.signals.push(SignalIr {
                        name: hidden.clone(),
                        width,
                        kind: SigKind::Wire,
                        init: Some(val),
                        mem: None,
                        signed: false,
                    });
                    self.names.insert(hidden.clone(), id);
                    self.sig_width.insert(id, width);
                    self.sig_rec.insert(id, tn.clone());
                    module
                        .real_ranges
                        .insert(hidden, self.record_real_ranges(&tn));
                }
                _ => {}
            }
        }
    }

    /// `c(i)` where `c` is an array-typed constant (e.g. a function-local
    /// constant, which cannot be a module signal because function bodies lower
    /// in slot space): lower to a `$const_idx` system call over the literal
    /// value — args are [flat value, flat index, element width].
    pub(super) fn lower_const_array_index(&mut self, fname: &str, args: &[Expr]) -> Option<Expr> {
        let tn = self.const_types.get(fname)?;
        let TypeInfo::Array { elem, dims, .. } = self.types.get(tn)? else {
            return None;
        };
        let val = self.consts.get(fname)?.clone();
        let flat = if !dims.is_empty() && dims.len() == args.len() {
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
            flat?
        } else {
            args.first()?.clone()
        };
        Some(Expr::Call {
            name: "$const_idx".into(),
            args: vec![
                Expr::Lit(val),
                flat,
                Expr::Lit(Value::from_u64(32, *elem as u64)),
            ],
        })
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

    /// Flatten a folded selected-name chain (`a.b(i).c`) into the root name
    /// (the first identifier) and the ordered segments after it. The parser
    /// folds the chain into nested Name/Call nodes: dotted segments stay
    /// flat in one Name, an index introduces a Call, and every `.field`
    /// after a Call wraps an outer Name.
    pub(super) fn flatten_selected_name(&self, e: &SyntaxNode, segs: &mut Vec<ChainSeg>) -> String {
        let mut root = String::new();
        for k in real_children(e) {
            match k.kind() {
                SyntaxKind::Ident => {
                    if root.is_empty() && segs.is_empty() {
                        root = self.kw(&k);
                    } else {
                        segs.push(ChainSeg::Field(self.kw(&k)));
                    }
                }
                SyntaxKind::Name => {
                    root = self.flatten_selected_name(&k, segs);
                }
                SyntaxKind::Call => {
                    let ck: Vec<SyntaxNode> = real_children(&k);
                    // The callee base is the first Name child; the index
                    // arguments are the remaining expression children.
                    let base_pos = ck.iter().position(|c| c.kind() == SyntaxKind::Name);
                    if let Some(bp) = base_pos {
                        root = self.flatten_selected_name(&ck[bp], segs);
                    }
                    let args: Vec<SyntaxNode> = ck
                        .iter()
                        .enumerate()
                        .filter(|(i, c)| Some(*i) != base_pos && is_expr_node(c.kind()))
                        .map(|(_, c)| c.clone())
                        .collect();
                    if !args.is_empty() {
                        segs.push(ChainSeg::Index(args));
                    }
                }
                _ => {}
            }
        }
        root
    }

    /// Resolve a selected-name chain's root: flatten the chain, reject
    /// function roots (a call's own lowering handles them) and empty
    /// chains, and classify the root — real signal (record, elem-record,
    /// or plain), constant folding from its value, a materialized hidden
    /// `__const_` signal, an alias, or a bare constant. Real signals win
    /// over constants (inner declarations shadow outer consts); only the
    /// materialized-constant fallback yields to the value.
    fn resolve_chain_root(
        &self,
        e: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) -> Option<(ChainRoot, Vec<ChainSeg>)> {
        let mut segs: Vec<ChainSeg> = Vec::new();
        let root = self.flatten_selected_name(e, &mut segs);
        if root.is_empty() || segs.is_empty() {
            return None;
        }
        let root_lc = root.to_ascii_lowercase();
        if self.funcs.contains_key(&root_lc) {
            return None;
        }
        let const_root = env
            .get(&root_lc)
            .cloned()
            .or_else(|| self.consts.get(&root_lc).cloned())
            .zip(self.const_types.get(&root_lc).cloned());
        let root = if let Some(id) = self.sig_id(&root) {
            let rec = self.sig_rec.get(&id).cloned();
            let elem_rec = self.sig_elem_rec.get(&id).cloned();
            match (rec, elem_rec) {
                (Some(rn), _) => ChainRoot::Walk {
                    cur: Expr::Sig(id),
                    cur_ty: rn,
                    root_elem: None,
                },
                (None, Some(en)) => ChainRoot::Walk {
                    cur: Expr::Sig(id),
                    cur_ty: en.clone(),
                    root_elem: Some(en),
                },
                _ => return const_root.map(|(v, _ty)| (ChainRoot::Lit(v), segs)),
            }
        } else if let Some((v, ty)) = const_root {
            ChainRoot::Walk {
                cur: Expr::Lit(v),
                cur_ty: ty,
                root_elem: None,
            }
        } else if let Some(id) = self.names.get(&format!("__const_{}", root_lc)).copied() {
            let rec = self.sig_rec.get(&id).cloned();
            let elem_rec = self.sig_elem_rec.get(&id).cloned();
            match (rec, elem_rec) {
                (Some(rn), _) => ChainRoot::Walk {
                    cur: Expr::Sig(id),
                    cur_ty: rn,
                    root_elem: None,
                },
                (None, Some(en)) => ChainRoot::Walk {
                    cur: Expr::Sig(id),
                    cur_ty: en.clone(),
                    root_elem: Some(en),
                },
                _ => return None,
            }
        } else if let Some(a) = self.aliases.get(&root_lc).cloned() {
            let ty = self.alias_rec.get(&root_lc).cloned()?;
            ChainRoot::Walk {
                cur: a,
                cur_ty: ty,
                root_elem: None,
            }
        } else {
            let v = self.consts.get(&root_lc).cloned()?;
            let ty = self.const_types.get(&root_lc).cloned()?;
            ChainRoot::Walk {
                cur: Expr::Lit(v),
                cur_ty: ty,
                root_elem: None,
            }
        };
        Some((root, segs))
    }

    /// Structural lowering of a selected-name chain rooted at a signal,
    /// constant, or alias: walk the segments, applying record field selects
    /// (slices of the flat record vector) and array element reads. Arrays
    /// inside records (and constant arrays) read as `$const_idx` over the
    /// flat value; whole signal arrays keep the kernel's memory `Idx`.
    /// Returns `None` when the chain does not resolve this way — the caller
    /// falls back to the legacy per-shape handling.
    pub(super) fn lower_selected_chain(
        &mut self,
        e: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) -> Option<Expr> {
        let (root, segs) = self.resolve_chain_root(e, env)?;
        match root {
            ChainRoot::Lit(v) => Some(Expr::Lit(v)),
            ChainRoot::Walk {
                cur,
                cur_ty,
                root_elem,
            } => self.walk_chain_segments(cur, cur_ty, root_elem, segs, env),
        }
    }

    /// Apply a record-field select segment: narrow the current expression to
    /// the field's bit range (fields packed first-declared = MSB) and step
    /// the type to the field's. The effective index dims come from the
    /// field's decl constraint, else the field type's own Array info.
    /// Returns the updated (expression, type, dims).
    fn chain_field_seg(
        &self,
        cur: Expr,
        cur_ty: String,
        f: &String,
    ) -> Option<(Expr, String, Option<Vec<(i64, usize)>>)> {
        let Some(TypeInfo::Record { fields }) = self.types.get(&cur_ty) else {
            return None;
        };
        let total: usize = fields.iter().map(|fd| fd.1).sum();
        let mut run = 0usize;
        let mut hit: Option<(usize, usize)> = None;
        for (fn_, w, _) in fields {
            let lsb = total - run - w;
            if *fn_ == *f {
                hit = Some((lsb, *w));
                break;
            }
            run += w;
        }
        let (lsb, w) = hit?;
        let outer_ty = cur_ty.clone();
        let field_ty = self.rec_field_ty.get(&(cur_ty.clone(), f.clone()))?.clone();
        let cur_dims = self
            .rec_field_dims
            .get(&(outer_ty, f.clone()))
            .cloned()
            .or_else(|| match self.types.get(&field_ty) {
                Some(TypeInfo::Array { dims, .. }) if !dims.is_empty() => Some(dims.clone()),
                _ => None,
            });
        let cur = Expr::Sel {
            msb: Box::new(Expr::Lit(Value::from_u64(32, (lsb + w - 1) as u64))),
            lsb: Box::new(Expr::Lit(Value::from_u64(32, lsb as u64))),
            e: Box::new(cur),
        };
        Some((cur, field_ty, cur_dims))
    }

    /// Apply an array-index segment. Memory-backed signal arrays index by
    /// element; flat value slots (function formals) read elements out of
    /// the flat vector — record elements via `$const_idx` (scalar elements
    /// keep the kernel's established bit read); non-signal bases index the
    /// flat constant value. Consumes the array-of-records element type,
    /// clears the dims context (a following Field seg re-derives it), and
    /// steps the type to the element's. Returns the updated (expr, type).
    fn chain_index_seg(
        &mut self,
        cur: Expr,
        cur_ty: String,
        root_elem: &mut Option<String>,
        cur_dims: &mut Option<Vec<(i64, usize)>>,
        args: &[SyntaxNode],
        env: &HashMap<String, Value>,
    ) -> Option<(Expr, String)> {
        // Array-of-records root: the element type is known from the slot
        // registration; other bases resolve it from the current type's
        // Array info.
        let root_ty = root_elem.take();
        let elem_rec_w = root_ty.as_ref().and_then(|en| match self.types.get(en) {
            Some(TypeInfo::Record { fields }) => {
                Some(fields.iter().map(|f| f.1).sum::<usize>().max(1))
            }
            _ => None,
        });
        let cur = if let Expr::Sig(sid) = cur {
            // Memory-backed signal arrays index by element; flat value
            // slots (function formals) read elements out of the flat
            // vector — record elements via `$const_idx` (scalar elements
            // keep the kernel's established bit read).
            let is_mem = self.mems.get(&sid).copied().is_some();
            if !is_mem && args.len() == 1 {
                if let Some(ew) = elem_rec_w {
                    let idx = self.lower_expr(&args[0], env);
                    *cur_dims = None;
                    return Some((
                        Expr::Call {
                            name: "$const_idx".into(),
                            args: vec![
                                Expr::Sig(sid),
                                idx,
                                Expr::Lit(Value::from_u64(32, ew as u64)),
                            ],
                        },
                        root_ty.unwrap_or(cur_ty),
                    ));
                }
            }
            if args.len() == 1 {
                let idx = self.lower_expr(&args[0], env);
                Expr::Idx {
                    e: Box::new(Expr::Sig(sid)),
                    i: Box::new(idx),
                }
            } else {
                let sd = self.sig_dims.get(&sid).cloned()?;
                if sd.len() != args.len() {
                    return None;
                }
                let flat = flatten_dims(args, &sd, env, self);
                Expr::Idx {
                    e: Box::new(Expr::Sig(sid)),
                    i: Box::new(flat),
                }
            }
        } else {
            let Some(TypeInfo::Array { elem, dims, .. }) = self.types.get(&cur_ty).cloned() else {
                return None;
            };
            // A record field's decl constraint overrides the (empty) dims
            // of an unconstrained array type; a single index with no dims
            // at all keeps the raw value (0-based assumption beats
            // dropping it).
            let dims = cur_dims.take().unwrap_or(dims);
            let flat = if args.len() == 1 && dims.is_empty() {
                self.lower_expr(&args[0], env)
            } else {
                flatten_dims(args, &dims, env, self)
            };
            Expr::Call {
                name: "$const_idx".into(),
                args: vec![
                    cur,
                    flat,
                    Expr::Lit(Value::from_u64(32, elem.max(1) as u64)),
                ],
            }
        };
        let cur_ty = match root_ty {
            Some(en) => en,
            None => match self.types.get(&cur_ty).cloned() {
                Some(TypeInfo::Array {
                    elem_name: Some(en),
                    ..
                }) => en,
                _ => return None,
            },
        };
        // The element itself is not the constrained field array anymore;
        // a following Field seg re-derives the dims.
        *cur_dims = None;
        Some((cur, cur_ty))
    }

    /// Walk a selected-name chain's segments from its resolved root,
    /// threading the current expression, type, array dims, and the
    /// array-of-records element type through each segment.
    fn walk_chain_segments(
        &mut self,
        cur: Expr,
        cur_ty: String,
        root_elem: Option<String>,
        segs: Vec<ChainSeg>,
        env: &HashMap<String, Value>,
    ) -> Option<Expr> {
        // Effective index dims of the current array context: the
        // type's own dims, overridden by a record field's decl
        // constraint (an unconstrained array type with `(0 to 3)`
        // at the field).
        let mut cur_dims: Option<Vec<(i64, usize)>> = match self.types.get(&cur_ty) {
            Some(TypeInfo::Array { dims, .. }) if !dims.is_empty() => Some(dims.clone()),
            _ => None,
        };
        let (mut cur, mut cur_ty, mut root_elem) = (cur, cur_ty, root_elem);
        for seg in segs {
            match seg {
                ChainSeg::Field(f) => {
                    (cur, cur_ty, cur_dims) = self.chain_field_seg(cur, cur_ty, &f)?;
                }
                ChainSeg::Index(args) => {
                    (cur, cur_ty) = self.chain_index_seg(
                        cur,
                        cur_ty,
                        &mut root_elem,
                        &mut cur_dims,
                        &args,
                        env,
                    )?;
                }
            }
        }
        Some(cur)
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
