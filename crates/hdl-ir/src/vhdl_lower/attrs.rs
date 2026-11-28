//! VHDL lowering: type-mark attributes — the `'length` family (with an
//! explicit dimension) and `t'image(x)`.
//!
//! `attr_dim_lookup` is the lookup order for dimension-suffixed attributes
//! (`'length(d)`, `'high(d)`, …): signal dims (`sig_dims`), object dims
//! (`obj_dims` — constants and generics), subtype dims (`type_dims`), array
//! type dims (`TypeInfo::Array`). The plain (unsuffixed) attribute form folds
//! through `obj_attr_expr` for constants/generics and `type_attr` for type
//! marks; dims for unconstrained array objects are recorded at the
//! declaration site from the initializer value's shape.
//!
//! Extracted verbatim from `expr.rs` and `vhdl_lower.rs` (pure move, no
//! logic changes); the object-dims additions are part of the same change.

use super::*;

impl<'a> Lower<'a> {
    pub(super) fn attr_dim_value(&self, base: &str, attr: &str, dim: usize) -> Option<u64> {
        let hit = self.attr_dim_lookup(base, attr, dim);
        if crate::debug::TRACE_LOWER.load(std::sync::atomic::Ordering::Relaxed) {
            let outcome = match &hit {
                Some((src, v)) => format!("{src} -> {v}"),
                None => "unresolved".to_string(),
            };
            eprintln!("[lower] {base}'{attr}({dim}): {outcome}");
        }
        hit.map(|(_, v)| v)
    }

    /// The lookup order for dimension-suffix attributes: the signal's own
    /// constraint dims, then object dims (constants/generics), then
    /// package-exported type dims, then the type's own array dims. A signal
    /// id short-circuits the chain even without an entry — a signal with no
    /// dims (unconstrained port) does not fall through to the type's dims.
    pub(super) fn attr_dim_lookup(
        &self,
        base: &str,
        attr: &str,
        dim: usize,
    ) -> Option<(&'static str, u64)> {
        if dim == 0 {
            return None;
        }
        let val = |(lo, sz): (i64, usize)| -> u64 { dim_attr_value(attr, lo, sz) };
        let blc = base.to_ascii_lowercase();
        if let Some(id) = self.sig_id(&blc) {
            return self
                .sig_dims
                .get(&id)
                .and_then(|d| d.get(dim - 1))
                .copied()
                .map(|d| ("sig_dims", val(d)));
        }
        if let Some(dims) = self.obj_dims.get(&blc) {
            return dims.get(dim - 1).copied().map(|d| ("obj_dims", val(d)));
        }
        let tn = blc.rsplit('.').next().unwrap_or("");
        if let Some(dims) = self.type_dims.get(tn) {
            return dims.get(dim - 1).copied().map(|d| ("type_dims", val(d)));
        }
        if let Some(TypeInfo::Array { dims, .. }) = self.types.get(tn) {
            return dims.get(dim - 1).copied().map(|d| ("type", val(d)));
        }
        None
    }

    pub(super) fn type_attr(&self, name: &str, attr: &str) -> Expr {
        let n = name.to_ascii_lowercase();
        // REAL bounds are f64 extremes and must carry the real flag: an X(32)
        // literal would read as X and — worse — fail overload width matching,
        // so `LOG(REAL'HIGH)` fell back to the first-registered (complex)
        // LOG overload and created a dispatch cycle. (LRM 1076 §3.1.4:
        // REAL'HIGH/REAL'LOW are the largest/smallest representable reals.)
        if n == "real" || matches!(self.types.get(&n), Some(TypeInfo::Real)) {
            return match attr {
                "high" | "right" => Expr::Lit(Value::from_f64(f64::MAX)),
                "low" | "left" => Expr::Lit(Value::from_f64(f64::MIN)),
                // 'length of a scalar type is 1.
                _ => Expr::Lit(Value::from_u64(32, 1)),
            };
        }
        let bounds: Option<(i64, i64)> = match n.as_str() {
            "integer" => Some((i32::MIN as i64, i32::MAX as i64)),
            "natural" | "positive" => Some((0, i32::MAX as i64)),
            "boolean" | "bit" | "std_logic" | "std_ulogic" => Some((0, 1)),
            _ => match self.types.get(&n) {
                Some(TypeInfo::Integer { .. }) => Some((i32::MIN as i64, i32::MAX as i64)),
                Some(TypeInfo::Enum(lits)) => Some((0, lits.len() as i64 - 1)),
                Some(TypeInfo::Array { dims, .. }) => {
                    dims.first().map(|(l, sz)| (*l, *l + *sz as i64 - 1))
                }
                // Vector subtypes: TypeInfo::Vector carries no constraint (the
                // base array is unconstrained); the width lives in
                // `type_widths` (declared subtypes and package-imported ones
                // alike). Bounds assume `0 to w-1` / `w-1 downto 0` — both
                // give low=0, high=w-1; only 'left/'right differ, and those
                // are rare in constant expressions. A constrained string
                // subtype (`subtype d is string(1 to 10)`) carries element
                // dims in `type_dims` — `'length` must be the element count
                // (10), not the bit width (80).
                Some(TypeInfo::Vector { .. }) => {
                    match self.type_dims.get(&n).and_then(|d| d.first()) {
                        Some((l, sz)) => Some((*l, *l + *sz as i64 - 1)),
                        None => self
                            .type_widths
                            .get(&n)
                            .copied()
                            .map(|w| (0, w.max(1) as i64 - 1)),
                    }
                }
                _ => None,
            },
        };
        let Some((lo, hi)) = bounds else {
            return Expr::Lit(Value::x(32)); // unconstrained vector etc.
        };
        let v = dim_attr_value(attr, lo, (hi - lo + 1).max(0) as usize);
        Expr::Lit(Value::from_u64(32, v))
    }

    /// Plain `'length`/`'high`/`'low`/`'left`/`'right` on a constant/generic:
    /// folds from the object's recorded `obj_dims` (first dimension), which
    /// the signal-centered plain-attribute path in `expr.rs` does not reach.
    /// Objects without dims return `None` and fall through to the type-mark
    /// lookup.
    pub(super) fn obj_attr_expr(&self, bn: &str, attr: &str) -> Option<Expr> {
        let (lo, sz) = *self.obj_dims.get(&bn.to_ascii_lowercase())?.first()?;
        Some(Expr::Lit(Value::from_u64(32, dim_attr_value(attr, lo, sz))))
    }

    /// Element width in bits of a one-dimensional array object type:
    /// declared arrays carry it in `TypeInfo::Array.elem`, std `string` is an
    /// array of 8-bit characters, other unconstrained vector subtypes are bit
    /// arrays. Multi-dimensional types return `None` — flat width divided by
    /// element width would describe only the innermost dimension.
    pub(super) fn array_elem_width(&self, tn: &str) -> Option<usize> {
        match self.types.get(tn) {
            Some(TypeInfo::Array { elem, depth: 1, .. }) => Some((*elem).max(1)),
            Some(TypeInfo::Array { .. }) => None,
            Some(TypeInfo::Vector { .. }) => Some(
                self.type_elem
                    .get(tn)
                    .copied()
                    .or_else(|| (tn == "string").then_some(8))
                    .unwrap_or(1),
            ),
            _ => None,
        }
    }

    /// Index shape of a record field that is itself an array
    /// (`r.options(0 to n)`): `(low bound, element width, field bit width)`.
    /// The low bound comes from the field's decl constraint
    /// (`rec_field_dims`) or the field type's own dims; the element width
    /// from the field type. `None` when the dotted name does not resolve to
    /// a record field of a multi-bit-element array type — the caller keeps
    /// the raw bounds.
    pub(super) fn field_array_shape(&self, name: &str) -> Option<(i64, u64, u64)> {
        let (root, rest) = name.split_once('.')?;
        let path: Vec<String> = rest.split('.').map(|s| s.to_ascii_lowercase()).collect();
        let last = path.last()?.clone();
        let id = self.sig_id(root)?;
        let mut ty = self.sig_rec.get(&id)?.clone();
        for seg in &path[..path.len() - 1] {
            ty = self.rec_field_ty.get(&(ty.clone(), seg.clone()))?.clone();
        }
        let (_, w) = self.record_field_offset(&ty, &[last.as_str()])?;
        let fty = self.rec_field_ty.get(&(ty.clone(), last.clone()))?.clone();
        let lo = self
            .rec_field_dims
            .get(&(ty, last))
            .and_then(|d| d.first().map(|&(l, _)| l))
            .or_else(|| match self.types.get(&fty) {
                Some(TypeInfo::Array { dims, .. }) => dims.first().map(|&(l, _)| l),
                _ => None,
            })
            // A constrained Vector subtype (`string(1 to 10)`) keeps its
            // range in `type_dims`.
            .or_else(|| {
                self.type_dims
                    .get(&fty)
                    .and_then(|d| d.first().map(|&(l, _)| l))
            })?;
        let ew = self.array_elem_width(&fty)? as u64;
        if ew <= 1 {
            return None;
        }
        Some((lo, ew, w as u64))
    }

    /// Whether the type mark names a character-array type (`string` or a
    /// string subtype): string literals in such an object's initializer are
    /// ASCII text, not bit strings (`lower_literal` consults this).
    pub(super) fn string_typed(&self, tm: &SyntaxNode) -> bool {
        let tn = self
            .kw(tm)
            .rsplit('.')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        tn == "string" || self.type_elem.get(&tn) == Some(&8)
    }

    /// Dims of an unconstrained one-dimensional array object derived from its
    /// initializer: an empty string literal (`""`) has length 0; otherwise
    /// element count = packed width / element width. An empty string lowers to
    /// one NUL byte (0-width values break packing downstream), so only the
    /// syntax reveals its length. An X value has no shape — returning dims for
    /// it would fold `'length` to a wrong number instead of X.
    pub(super) fn dims_from_init(
        &self,
        tm: Option<&SyntaxNode>,
        init: Option<&SyntaxNode>,
        v: &Value,
    ) -> Option<Vec<(i64, usize)>> {
        if init.is_some_and(|n| self.text(n) == "\"\"") {
            return Some(vec![(0, 0)]);
        }
        let tm = tm?;
        let tn_full = self.kw(tm);
        let tn = tn_full.rsplit('.').next().unwrap_or("");
        let elem = self.array_elem_width(tn)?;
        if v.width() == 0 {
            return Some(vec![(0, 0)]);
        }
        // An X value has no shape: `'length` must fold to X, not a number.
        v.to_u64()?;
        let (n, _lo) = self.array_agg_len(v.width(), elem);
        let n = n?;
        // A string-typed object initialized from a literal takes the
        // literal's range, `1 TO n` (LRM 7.3.1 string literal index
        // subtype) — element addressing `s(k)` is 1-based off this low
        // bound. Non-string array types keep the element-count shape:
        // their declared range (possibly descending) is unknown here.
        let lo = if elem == 8 { 1 } else { 0 };
        Some(vec![(lo, n)])
    }

    /// Register `obj_dims` for a constant from its initializer (see
    /// `dims_from_init`), under the same `--trace-lower` trail as
    /// `set_sig_dims_from_constraint`. Existing entries win: an inline
    /// constraint at the declaration is more specific than the value's shape.
    pub(super) fn set_obj_dims_from_init(
        &mut self,
        name: &str,
        tm: Option<&SyntaxNode>,
        init: Option<&SyntaxNode>,
        v: &Value,
    ) {
        if self.obj_dims.contains_key(name) {
            return;
        }
        if let Some(dims) = self.dims_from_init(tm, init, v) {
            if crate::debug::TRACE_LOWER.load(std::sync::atomic::Ordering::Relaxed) {
                eprintln!("[lower] obj_dims[{name}] = {dims:?} via initializer");
            }
            // Value-shape dims address MSB-first direct (element k = value
            // element k): the ascending mapping, so a recorded decl
            // constraint's direction wins and stays untouched.
            self.obj_asc
                .entry(name.to_string())
                .or_insert(vec![true; dims.len()]);
            self.obj_dims.insert(name.to_string(), dims);
        }
    }

    /// Element count and flat low bound for an array aggregate derived from
    /// a width that already includes the constraint (record field width):
    /// `n = width / element_width`. Decl-site ranges lose their low bound at
    /// this level — flat positions assume it is the low index;
    /// `others`-only and positional aggregates are unaffected.
    pub(super) fn array_agg_len(&self, fw: usize, elem: usize) -> (Option<usize>, i64) {
        let elem = elem.max(1);
        if fw >= elem && fw % elem == 0 {
            (Some(fw / elem), 0)
        } else {
            (None, 0)
        }
    }

    /// Low bound of a declaration constraint's first range
    /// (`(0 to 3)` → 0, `(2 downto 1)` → 2), const-folded.
    pub(super) fn range_low(
        &mut self,
        c: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) -> Option<i64> {
        let sub = real_children(c)
            .into_iter()
            .find(|k| k.kind() == SyntaxKind::Range)?;
        let first = real_children(&sub)
            .into_iter()
            .find(|k| is_expr_node(k.kind()))?;
        let ex = self.lower_expr(&first, env);
        self.ce_env(&ex, env)
            .and_then(|v| v.to_u64())
            .map(|v| v as i64)
    }

    /// Low bound of a constraint's first range, folding only that bound with
    /// the registration env — a dynamic high bound (`string(1 to size)`)
    /// defeats a whole-range fold, and the direction decides which
    /// expression is the low bound (the left one for `to`, the right one for
    /// `downto`; `range_low` always returns the left bound, which is the
    /// HIGH bound of a descending range).
    pub(super) fn range_bound_low(&mut self, c: &SyntaxNode, asc: bool) -> Option<i64> {
        let sub = real_children(c)
            .into_iter()
            .find(|k| k.kind() == SyntaxKind::Range)?;
        let exprs: Vec<SyntaxNode> = real_children(&sub)
            .into_iter()
            .filter(|k| is_expr_node(k.kind()))
            .collect();
        let node = if asc { exprs.first() } else { exprs.last() }?;
        let ex = self.lower_expr(node, &self.consts.clone());
        self.ce_env(&ex, &self.consts.clone())
            .and_then(|v| v.to_i64())
    }

    /// Dims of a declaration constraint: one (low, size) per range, left to
    /// right. Unconstrained (`<>`) ranges and unresolvable bounds count as
    /// size 1 — a wrong large size would mislayout aggregates.
    pub(super) fn constraint_dims(&mut self, c: &SyntaxNode) -> Vec<(i64, usize)> {
        constraint_ranges(c)
            .iter()
            .map(|r| self.constraint_range_dim(r))
            .collect()
    }

    /// Ascending flag per dim of an index constraint, aligned with
    /// `constraint_dims`' range order: element k of an ascending range is
    /// value element k (MSB-first direct), of a descending range value
    /// element size-1-k (the flat index flips). A range without keywords
    /// defaults to descending, matching `bounds_dim`.
    pub(super) fn constraint_dirs(&self, c: &SyntaxNode) -> Vec<bool> {
        constraint_ranges(c)
            .iter()
            .map(|r| !self.text(r).to_ascii_lowercase().contains("downto"))
            .collect()
    }

    /// Record object shape (dims + per-dim directions) for every name of a
    /// constant declaration whose type mark resolves to an array-like type
    /// with a concrete inline index constraint — the constraint is not part
    /// of the type mark, so neither is derivable at use sites. A `<>`
    /// (unconstrained) constraint records nothing.
    pub(super) fn record_const_shapes(
        &mut self,
        tm: Option<&SyntaxNode>,
        constraint: Option<&SyntaxNode>,
        names: &[String],
    ) {
        let (Some(tm), Some(c)) = (tm, constraint) else {
            return;
        };
        let tn = self
            .kw(tm)
            .rsplit('.')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        if !matches!(
            self.types.get(&tn),
            Some(TypeInfo::Array { .. } | TypeInfo::Vector { .. })
        ) || self.text(c).contains("<>")
        {
            return;
        }
        for n in names {
            self.record_const_shape(&n.to_ascii_lowercase(), c);
        }
    }

    /// Record a constant's object shape from its inline index constraint —
    /// dims (`obj_dims`, for `'length(d)`-style folds) and per-dim
    /// directions (`obj_asc`, for index/slice addressing). The constraint is
    /// not part of the type mark, so neither is derivable at use sites.
    pub(super) fn record_const_shape(&mut self, name: &str, c: &SyntaxNode) {
        let cd = self.constraint_dims(c);
        if !cd.is_empty() {
            self.obj_dims.insert(name.to_string(), cd);
        }
        let dirs = self.constraint_dirs(c);
        if !dirs.is_empty() {
            self.obj_asc.insert(name.to_string(), dirs);
        }
    }

    /// (low, size) of one constraint range: an enum type name gives its
    /// literal count; otherwise the bounds const-fold.
    fn constraint_range_dim(&mut self, r: &SyntaxNode) -> (i64, usize) {
        if self.text(r).contains("<>") {
            return (0, 1);
        }
        let rk = real_children(r);
        if let Some(n) = rk.iter().find(|k| k.kind() == SyntaxKind::Name) {
            let tn = self.text(n).to_ascii_lowercase();
            match self.types.get(&tn) {
                Some(TypeInfo::Enum(lits)) => return (0, lits.len()),
                Some(_) => return (0, 1),
                None => {}
            }
        }
        let exprs: Vec<Expr> = rk
            .iter()
            .filter(|k| is_expr_node(k.kind()))
            .map(|k| self.lower_expr(k, &HashMap::new()))
            .collect();
        match exprs.len() {
            0 => (0, 1),
            1 => (
                0,
                self.ce_env(&exprs[0], &HashMap::new())
                    .and_then(|v| v.to_u64())
                    .unwrap_or(1) as usize,
            ),
            _ => self.bounds_dim(r, &exprs),
        }
    }

    /// (low, size) from a range's two folded bounds. Direction decides
    /// low/high: `5 to -1` (e.g. an under-evaluated `0 to N-1`) is a null
    /// range of size 0, not the descending range -1 to 5 — swapping bounds
    /// without the keyword turned a null range into 2³² words and OOM-killed
    /// the run. VHDL keywords are case-insensitive — match `DOWNTO`/`downto`.
    fn bounds_dim(&mut self, r: &SyntaxNode, exprs: &[Expr]) -> (i64, usize) {
        let a = self
            .ce_env(&exprs[0], &HashMap::new())
            .and_then(|v| v.to_i64());
        let b = self
            .ce_env(&exprs[1], &HashMap::new())
            .and_then(|v| v.to_i64());
        if let (Some(a), Some(b)) = (a, b) {
            let descending = self.text(r).to_ascii_lowercase().contains("downto");
            let (low, high) = if descending { (b, a) } else { (a, b) };
            let size = if high >= low {
                (high as i128 - low as i128 + 1).min(usize::MAX as i128) as usize
            } else {
                0
            };
            (low, size)
        } else {
            (0, 1)
        }
    }
    /// Shape of a positional aggregate: element count per nesting level
    /// (`((1,2,3),(4,5,6))` → `[(0, 2), (0, 3)]`). Low bounds are unknown
    /// for unconstrained bases — 0 is enough for `'length(d)`-style
    /// attributes, which only need the sizes.
    pub(super) fn aggregate_dims(&self, agg: &SyntaxNode) -> Vec<(i64, usize)> {
        let elems: Vec<SyntaxNode> = real_children(agg)
            .into_iter()
            .filter(|k| is_expr_node(k.kind()))
            .collect();
        let n = elems.len();
        if n == 0 {
            return Vec::new();
        }
        let mut dims = vec![(0i64, n)];
        if let Some(first) = elems.iter().find(|k| k.kind() == SyntaxKind::Aggregate) {
            dims.extend(self.aggregate_dims(first));
        }
        dims
    }

    /// Insert `sig_dims`, tracing the population under `--trace-lower`
    /// (the population half of the `'length(d)` diagnosis trail — the
    /// duplicate-warning-class regressions lived exactly at these sites).
    pub(super) fn set_sig_dims_from_constraint(
        &mut self,
        id: SigId,
        dims: Vec<(i64, usize)>,
        name: &str,
    ) {
        if crate::debug::TRACE_LOWER.load(std::sync::atomic::Ordering::Relaxed) {
            eprintln!("[lower] sig_dims[{id}] = {dims:?} via inline constraint ({name})");
        }
        self.sig_dims.insert(id, dims);
    }
}

/// The index constraint's ranges (or the node itself when it is a bare
/// range), shared by `constraint_dims` and `constraint_dirs`.
fn constraint_ranges(c: &SyntaxNode) -> Vec<SyntaxNode> {
    let subs: Vec<SyntaxNode> = real_children(c)
        .into_iter()
        .filter(|k| k.kind() == SyntaxKind::Range)
        .collect();
    if subs.is_empty() {
        vec![c.clone()]
    } else {
        subs
    }
}

/// Shared bound→value mapping for the `'length`/`'high`/`'low`/`'left`/`'right`
/// family given a dimension's low bound and size: `high`/`left` is the last
/// index, clamped at 0 for empty dimensions.
fn dim_attr_value(attr: &str, lo: i64, sz: usize) -> u64 {
    match attr {
        "length" => sz as u64,
        "high" | "left" => (lo + sz as i64 - 1).max(0) as u64,
        _ => lo.max(0) as u64, // low | right
    }
}

impl<'a> Lower<'a> {
    /// Lower `t'image(x)` for the argument node `arg` and the type-mark
    /// `prefix` the attribute is applied to.
    pub(super) fn attr_image_expr(
        &mut self,
        arg: &SyntaxNode,
        env: &HashMap<String, Value>,
        prefix: &str,
    ) -> Expr {
        let e = self.lower_expr(arg, env);
        // `boolean'image` spells `true`/`false`, not the decimal bit value
        // every `ToString` evaluator renders. Fold the constant case here;
        // non-constant arguments render in the evaluator (`ToString`'s
        // decimal fallback reads as -1/1/0). Only genuinely 1-bit values
        // qualify — a wider operand under a `boolean` prefix (loose typing,
        // e.g. a natural) keeps the decimal rendering.
        if prefix.eq_ignore_ascii_case("boolean") {
            if let Some(v) = self.ce(&e).filter(|v| v.width() == 1) {
                let s = if v.to_u64().unwrap_or(0) & 1 == 1 {
                    "true"
                } else {
                    "false"
                };
                return Expr::Lit(string_value(s));
            }
            return Expr::ToString {
                e: Box::new(e),
                boolean: true,
            };
        }
        // A signed-integer image renders two's complement
        // (`integer'image(-1)` is "-1"): mark the operand so the
        // evaluator sign-extends instead of printing raw bits.
        let e = if matches!(
            self.types.get(&prefix.to_ascii_lowercase()),
            Some(TypeInfo::Integer { signed: true })
        ) {
            Expr::Signed(Box::new(e))
        } else {
            e
        };
        Expr::ToString {
            e: Box::new(e),
            boolean: false,
        }
    }
}
