//! Width and constraint resolution: constraint/range width folding, ascending detection, record object shapes, and expression width estimation for fallback sizing.
//!
//! Extracted verbatim from vhdl_lower.rs (pure move, no logic changes).

use super::*;

impl<'a> Lower<'a> {
    /// Row width for a memory-backed object (`mem` set): `resolve_type`
    /// returns an array type mark's full flat width, but a memory stores
    /// `depth` rows of element width, so the slot must stay one row.
    pub(super) fn mem_row_width(&self, tm: Option<&SyntaxNode>, width: usize) -> usize {
        let Some(tm) = tm else {
            return width;
        };
        let tn = self
            .kw(tm)
            .rsplit('.')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        match self.types.get(&tn) {
            Some(TypeInfo::Array { elem, .. }) => (*elem).max(1),
            _ => width,
        }
    }

    /// Whether this node references a generic that is currently bound to no
    /// value (an unbound formal). Expressions hanging on such a generic are
    /// constant per instance — the generic map rebinds it — so their
    /// bare-context fold failure warns misleadingly and callers skip the
    /// warning.
    pub(super) fn refs_unset_generic(&self, n: &SyntaxNode) -> bool {
        n.descendants()
            .filter(|d| matches!(d.kind(), SyntaxKind::Name | SyntaxKind::Ident))
            .any(|d| {
                let t = self.text(&d).to_ascii_lowercase();
                // Selected names (`pkg.G_SIZE`) match on their last segment;
                // the currently-unknown gate keeps stale markers from other
                // contexts harmless.
                let last = t.rsplit('.').next().unwrap_or(&t);
                [t.as_str(), last].iter().any(|cand| {
                    self.per_instance_names.contains(*cand)
                        && matches!(self.consts.get(*cand), Some(v) if !v.is_known())
                })
            })
    }

    /// Width from a constraint: `(msb downto lsb)` / `(msb to lsb)` / `(n)`,
    /// or multi-dimensional `(r1, r2, ...)` → product of the dimension widths.
    pub(super) fn constraint_width(&mut self, c: &SyntaxNode) -> usize {
        let kids = real_children(c);
        let subs: Vec<SyntaxNode> = kids
            .into_iter()
            .filter(|k| k.kind() == SyntaxKind::Range)
            .collect();
        if !subs.is_empty() {
            return subs
                .iter()
                .map(|s| self.range_width(s))
                .fold(1usize, |a, b| a * b.max(1))
                .max(1);
        }
        self.range_width(c)
    }

    /// Width of a single range: `a to/downto b`, `(n)`, box `<>`, or an
    /// index given as a type mark (`array (enum_t) of ...` → literal count).
    pub(super) fn range_width(&mut self, r: &SyntaxNode) -> usize {
        // Box constraint `type range <>` — genuinely unconstrained; do not try
        // to evaluate the index type name (it would report "unknown identifier").
        if self.text(r).contains("<>") {
            return 1;
        }
        let kids = real_children(r);
        // Index given as a type mark: the depth is the enum's literal count.
        // A name that is a *constant* (not a type) still goes through the
        // expression path below.
        if let Some(n) = kids.iter().find(|k| k.kind() == SyntaxKind::Name) {
            let tn = self.text(n).to_ascii_lowercase();
            match self.types.get(&tn) {
                Some(TypeInfo::Enum(lits)) => return lits.len().max(1),
                Some(_) => return 1,
                None => {}
            }
        }
        let exprs: Vec<Expr> = kids
            .iter()
            .filter(|k| is_expr_node(k.kind()))
            .map(|k| self.lower_expr(k, &HashMap::new()))
            .collect();
        if exprs.len() >= 2 {
            if let (Some(a), Some(b)) = (
                self.ce_env(&exprs[0], &HashMap::new())
                    .and_then(|v| v.to_i64()),
                self.ce_env(&exprs[1], &HashMap::new())
                    .and_then(|v| v.to_i64()),
            ) {
                // Direction decides high/low (see `constraint_dims`): a null
                // range is width 0, never an underflowed 2³². VHDL keywords
                // are case-insensitive — match `DOWNTO`/`downto`.
                let descending = self.text(r).to_ascii_lowercase().contains("downto");
                let (low, high) = if descending { (b, a) } else { (a, b) };
                return if high >= low {
                    (high as i128 - low as i128 + 1).min(usize::MAX as i128) as usize
                } else {
                    0
                };
            }
            1
        } else if exprs.len() == 1 {
            self.ce_env(&exprs[0], &HashMap::new())
                .and_then(|v| v.to_u64())
                .map(|n| n as usize)
                .unwrap_or(1)
        } else {
            1
        }
    }

    /// Dimensions of an array constraint: [(low, size), …] per index range.
    /// Direction of a vector's own index constraint (`(0 to N-1)` vs
    /// `(N-1 downto 0)`) — the flat bit order of ascending vectors is
    /// mirrored, so slices on them must remap (see the `PartSelect` arm).
    pub(super) fn constraint_ascending(&self, c: &SyntaxNode) -> Option<bool> {
        let kids = real_children(c);
        let r = kids
            .iter()
            .find(|k| k.kind() == SyntaxKind::Range)
            .unwrap_or(c);
        let dir = real_children(r)
            .iter()
            .find(|k| k.kind() == SyntaxKind::Ident)
            .map(|k| self.kw(k))?;
        match dir.as_str() {
            "downto" => Some(false),
            "to" => Some(true),
            _ => None,
        }
    }

    /// Record an object's index-constraint direction (`sig_asc`) and element
    /// width (`sig_elem`) from its type mark and constraint, so index/slice
    /// addressing can remap ascending ranges and scale multi-bit elements
    /// (`string` characters). Both maps are lower-side addressing aids only.
    pub(super) fn record_obj_shape(
        &mut self,
        id: SigId,
        type_mark: Option<&SyntaxNode>,
        constraint: Option<&SyntaxNode>,
    ) {
        if let Some(c) = constraint {
            if let Some(asc) = self.constraint_ascending(c) {
                // The low bound must fold on its own — a dynamic high bound
                // (`string(1 to size)`) defeats a whole-constraint fold, and
                // `constraint_dims` folds bounds with an empty env, missing
                // formal-dependent ones. Fold the direction's low bound
                // directly; fall back to the whole-constraint dims, then 0.
                let lo = self
                    .range_bound_low(c, asc)
                    .or_else(|| self.constraint_dims(c).first().map(|&(l, _)| l))
                    .unwrap_or(0);
                self.sig_asc.insert(id, (lo, asc));
            }
        }
        if let Some(tm) = type_mark {
            let tn = self
                .kw(tm)
                .rsplit('.')
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            if let Some(ew) = self.array_elem_width(&tn) {
                if ew > 1 {
                    self.sig_elem.insert(id, ew);
                }
            }
            if tn == "string" && !self.sig_asc.contains_key(&id) {
                // Predefined `string` is positive-range ascending, and an
                // unconstrained formal takes the actual's range — (1 to n)
                // for the literal and constrained-object actuals this kernel
                // produces. Without a recorded low bound the slice remap
                // defaults to (0, descending) and reads a 1-based actual one
                // element off (`arg(1 to 4)` yielding chars 2..5).
                self.sig_asc.insert(id, (1, true));
            }
        }
    }

    /// Whether an index constraint's bounds const-fold at registration time.
    /// Formals have no values yet, so a constraint naming one does not fold.
    /// Whether the type mark resolves to an array-like type whose flat width
    /// an index constraint can change (`string`, vectors, arrays). A record's
    /// flat width is the sum of its fields — a record-element constraint
    /// (`t_node(name(1 to n))`) selects field ranges only, so the dyn-width
    /// machinery must not touch it.
    pub(super) fn constraint_shapes_width(&self, tm: &SyntaxNode) -> bool {
        let tn = self
            .kw(tm)
            .rsplit('.')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        matches!(
            self.types.get(&tn),
            Some(TypeInfo::Vector { .. }) | Some(TypeInfo::Array { .. })
        )
    }

    pub(super) fn constraint_folds(&mut self, c: &SyntaxNode) -> bool {
        if self.text(c).contains("<>") {
            return true; // box: unconstrained by intent, not unfoldable
        }
        // The node may be the parenthesized group `(1 to 4)` — descend to
        // the nested Range(s) like `constraint_width` does.
        let subs: Vec<SyntaxNode> = real_children(c)
            .into_iter()
            .filter(|k| k.kind() == SyntaxKind::Range)
            .collect();
        let ranges: Vec<SyntaxNode> = if subs.is_empty() {
            vec![c.clone()]
        } else {
            subs
        };
        let env = self.consts.clone();
        ranges.iter().all(|r| {
            let exprs: Vec<Expr> = real_children(r)
                .into_iter()
                .filter(|k| is_expr_node(k.kind()))
                .map(|k| self.lower_expr(&k, &env))
                .collect();
            !exprs.is_empty()
                && exprs
                    .iter()
                    .all(|e| self.ce_env(e, &env).and_then(|v| v.to_i64()).is_some())
        })
    }

    /// Estimated bit width of a lowered expression (lowerer-side; used to give
    /// unconstrained return types the width of what is actually returned).
    pub(super) fn expr_width_est(&self, e: &Expr) -> usize {
        match e {
            Expr::Lit(v) => v.width(),
            Expr::Real(_) => 64,
            Expr::Sig(s) => {
                if self.sig_dynw.contains_key(s) {
                    // A dynamic-width subprogram local's registration width
                    // is a fallback, not a bound: the unconstrained-return
                    // fixup must keep the historical 32-slot convention
                    // (the real width is per-call, `FuncIr.dyn_locals`).
                    32
                } else {
                    self.sig_width.get(s).copied().unwrap_or(32).max(1)
                }
            }
            Expr::Bin { l, r, .. } => self.expr_width_est(l).max(self.expr_width_est(r)),
            Expr::Un { e, .. } => self.expr_width_est(e),
            Expr::Cond { t, e, .. } => self.expr_width_est(t).max(self.expr_width_est(e)),
            Expr::Concat(parts) => parts
                .iter()
                .map(|p| self.expr_width_est(p))
                .sum::<usize>()
                .max(1),
            Expr::Sel { msb, lsb, .. } => {
                let a = self.ce(msb).and_then(|v| v.to_u64()).unwrap_or(0) as usize;
                let b = self.ce(lsb).and_then(|v| v.to_u64()).unwrap_or(0) as usize;
                a.abs_diff(b) + 1
            }
            Expr::Idx { e, .. } => self.expr_width_est(e),
            Expr::Call { name, args } => self
                .funcs
                .get(&name.to_ascii_lowercase())
                .and_then(|c| {
                    // Overloads: prefer the candidate with this call's
                    // formal count (ret_width only depends on the body).
                    c.iter()
                        .find(|f| f.args.len() == args.len())
                        .or_else(|| c.first())
                })
                .map(|f| f.ret_width)
                .unwrap_or(32)
                .max(1),
            Expr::Replicate { count, e } => {
                self.ce(count).and_then(|v| v.to_u64()).unwrap_or(1) as usize
                    * self.expr_width_est(e)
            }
            Expr::Resize { width, .. } => *width,
            Expr::Signed(e) => self.expr_width_est(e),
            Expr::ToString(_) => 8 * 12,
            _ => 32,
        }
    }
}
