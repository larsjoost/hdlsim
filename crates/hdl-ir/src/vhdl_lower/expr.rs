//! VHDL expression lowering: expressions, calls, attribute access,
//! literals, and aggregates. Extracted verbatim from the lowerer body
//! (pure move, no logic changes); decomposition into per-kind helpers
//! follows in separate changes.

use super::*;

mod aggregate;
mod attr_expr;
mod call;
mod dispatch;
mod literal;
mod name;
mod operators;
mod part_sel;

impl<'a> Lower<'a> {
    pub(super) fn lower_expr(&mut self, e: &SyntaxNode, env: &HashMap<String, Value>) -> Expr {
        match e.kind() {
            SyntaxKind::Literal => self.lower_literal(e, env),
            SyntaxKind::ParenExpr => {
                let inner = real_children(e)
                    .into_iter()
                    .find(|k| is_expr_node(k.kind()));
                match inner {
                    Some(k) => self.lower_expr(&k, env),
                    None => Expr::Lit(Value::x(1)),
                }
            }
            SyntaxKind::Qualified => self.lower_qualified(e, env),
            // VHDL-2008 conditional expression `v when c else e` (children:
            // value, condition, else) or the `??` operator (children: value,
            // condition).
            SyntaxKind::CondExpr => {
                let kids: Vec<SyntaxNode> = real_children(e)
                    .into_iter()
                    .filter(|k| is_expr_node(k.kind()))
                    .collect();
                match kids.as_slice() {
                    [v, c, el] => Expr::Cond {
                        c: Box::new(self.lower_expr(c, env)),
                        t: Box::new(self.lower_expr(v, env)),
                        e: Box::new(self.lower_expr(el, env)),
                    },
                    [v, c] => Expr::Cond {
                        c: Box::new(self.lower_expr(c, env)),
                        t: Box::new(self.lower_expr(v, env)),
                        e: Box::new(Expr::Lit(Value::x(32))),
                    },
                    _ => Expr::Lit(Value::x(32)),
                }
            }
            SyntaxKind::Name => self.lower_name(e, env),
            SyntaxKind::BinExpr => self.lower_bin_expr(e, env),
            SyntaxKind::UnaryExpr => self.lower_unary_expr(e, env),
            SyntaxKind::Call => self.lower_call(e, env),
            SyntaxKind::PartSelect => self.lower_part_select(e, env),
            SyntaxKind::Attr => self.lower_attr(e),
            SyntaxKind::Aggregate => {
                let w = self.agg_target_w.unwrap_or(32);
                self.lower_aggregate(e, env, w)
            }
            // VHDL-2008 external name in expression position
            // (`q <= << signal dut.s : std_logic >>;`): the hierarchical
            // target is not simulated — placeholder, no diagnostic.
            SyntaxKind::ExtName => Expr::Lit(Value::x(1)),
            _ => {
                self.err(e.span(), format!("unsupported expression {:?}", e.kind()));
                Expr::Lit(Value::x(1))
            }
        }
    }

    /// A call's shape binds: for every formal position that is shape-bound
    /// in any same-name candidate, resolve the actual's index range in the
    /// caller's context (see `actual_shape` for the per-actual cases).
    /// Positions with an unresolvable actual omit the bind.
    pub(super) fn call_shape_binds(
        &self,
        fname: &str,
        args: &[Expr],
        nodes: &[SyntaxNode],
    ) -> Vec<ShapeBind> {
        let Some(cands) = self.funcs.get(fname) else {
            return Vec::new();
        };
        let mut positions: Vec<usize> = cands
            .iter()
            .flat_map(|f| f.hidden.iter().map(|h| h.formal))
            .collect();
        positions.sort_unstable();
        positions.dedup();
        let mut binds = Vec::new();
        for p in positions {
            let Some(a) = args.get(p) else {
                continue;
            };
            let Some((lo, asc)) = self.actual_shape(a, nodes.get(p)) else {
                continue;
            };
            binds.push(ShapeBind { formal: p, lo, asc });
        }
        binds
    }

    /// One actual's index range in the caller's context, best-effort: an
    /// outer shape-bound formal chains its own hidden slots; a recorded
    /// shape (`sig_asc`) lowers to literals; a folded constant actual
    /// carries no id, so its declared range comes from the recorded object
    /// shape (`obj_dims`/`obj_asc`) under the actual's name; a constrained
    /// formal actual keeps its raw (0, descending) convention. `None` omits
    /// the bind — the callee falls back to its recorded convention, which
    /// is the pre-shape-binding behavior bit for bit.
    fn actual_shape(&self, a: &Expr, node: Option<&SyntaxNode>) -> Option<(Expr, Expr)> {
        match a {
            Expr::Sig(s) => {
                if let Some(&(flo, fasc)) = self.formal_dyn.get(s) {
                    return Some((Expr::Sig(flo), Expr::Sig(fasc)));
                }
                if let Some(&(lo, asc)) = self.sig_asc.get(s) {
                    return Some((
                        Expr::Lit(Value::from_u64(32, lo.max(0) as u64)),
                        Expr::Lit(Value::from_u64(32, asc as u64)),
                    ));
                }
                // A constrained formal actual (subtype-typed, no constraint
                // node): its addressing convention is the raw (0, descending).
                self.fn_formals.contains(s).then(|| {
                    (
                        Expr::Lit(Value::from_u64(32, 0)),
                        Expr::Lit(Value::from_u64(32, 0)),
                    )
                })
            }
            // Folded constant: the recorded object shape under the actual's
            // name.
            _ => {
                let key = self.obj_name(node);
                let &(lo, _) = self.obj_dims.get(&key).and_then(|d| d.first())?;
                let asc = self
                    .obj_asc
                    .get(&key)
                    .and_then(|d| d.first().copied())
                    .unwrap_or(false);
                Some((
                    Expr::Lit(Value::from_u64(32, lo.max(0) as u64)),
                    Expr::Lit(Value::from_u64(32, asc as u64)),
                ))
            }
        }
    }

    /// The recorded-shape lookup key of an actual node: the node's full
    /// (lowercased) text, falling back to its last dotted segment —
    /// `obj_dims`/`obj_asc` are keyed by bare object name.
    fn obj_name(&self, n: Option<&SyntaxNode>) -> String {
        let Some(n) = n else {
            return String::new();
        };
        let t = self.text(n).to_ascii_lowercase();
        match t.rsplit_once('.') {
            Some((_, last)) => last.to_string(),
            None => t,
        }
    }

    /// Thin dispatcher for [`Self::typed_record_aggregate`] (defined in the
    /// `aggregate` child module); keeps the `pub(super)` call sites in
    /// `decls.rs` unchanged.
    pub(super) fn lower_typed_record_aggregate(
        &mut self,
        e: &SyntaxNode,
        tn: &str,
        fields: &[(String, usize, usize)],
        env: &HashMap<String, Value>,
    ) -> Expr {
        self.typed_record_aggregate(e, tn, fields, env)
    }

    /// Thin dispatcher for [`Self::array_aggregate_specs`] (defined in the
    /// `aggregate` child module); keeps the `pub(super)` call sites in
    /// `decls.rs` and `aggregate.rs` unchanged.
    pub(super) fn lower_array_aggregate_specs(
        &mut self,
        e: &SyntaxNode,
        n: usize,
        lo: i64,
        elem: usize,
        elem_name: Option<String>,
        env: &HashMap<String, Value>,
    ) -> Option<Expr> {
        self.array_aggregate_specs(e, n, lo, elem, elem_name, env)
    }
}
