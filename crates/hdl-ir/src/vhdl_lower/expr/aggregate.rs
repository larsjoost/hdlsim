//! VHDL aggregate lowering: positional/`others` array aggregates and record
//! aggregates with typed field folding (nested record/array aggregates keep
//! their layout instead of degrading to X). Split out of `expr.rs` to keep
//! every function at its standards baseline; `typed_record_aggregate` and
//! `array_aggregate_specs` are also called from `decls.rs` and `aggregate.rs`
//! (initializers) through same-visibility wrappers on `expr.rs`.

use super::*;

/// Classified array-aggregate elements: named index choices (choice value,
/// value-node index), positional value indexes, and the `others` value index.
struct AggScan {
    named: Vec<(i64, usize)>,
    positional: Vec<usize>,
    others: Option<usize>,
}

impl<'a> Lower<'a> {
    /// True when the element at `kids[i]` is followed by the `=>`
    /// association arrow (a choice, not a positional value).
    fn is_assoc_arrow(&self, kids: &[SyntaxNode], i: usize) -> bool {
        kids.get(i + 1)
            .map(|p| p.kind() == SyntaxKind::Punct && self.text(p) == "=")
            .unwrap_or(false)
            && kids
                .get(i + 2)
                .map(|p| p.kind() == SyntaxKind::Punct && self.text(p) == ">")
                .unwrap_or(false)
    }

    pub(super) fn lower_aggregate(
        &mut self,
        e: &SyntaxNode,
        env: &HashMap<String, Value>,
        width: usize,
    ) -> Expr {
        // `(others => x)` → replicate x; `(a, b, c)` → concat.
        let kids = real_children(e);
        if kids.iter().any(|k| k.kind() == SyntaxKind::Others) {
            return self.others_replicate(&kids, env, width);
        }
        // Positional values only: a choice followed by `=>` is an association
        // name, not a value (record-typed targets use lower_record_aggregate).
        let mut parts: Vec<Expr> = Vec::new();
        let mut i = 0;
        while i < kids.len() {
            let k = &kids[i];
            if is_expr_node(k.kind()) && !self.is_assoc_arrow(&kids, i) {
                parts.push(self.lower_expr(k, env));
            }
            i += 1;
        }
        Expr::Concat(parts)
    }

    /// `(others => x)` → replicate the value by elements: a character value
    /// (`' '`) is 8 bits, a std_logic value 1 — the target `width` is total
    /// bits, so the count is width / element width.
    fn others_replicate(
        &mut self,
        kids: &[SyntaxNode],
        env: &HashMap<String, Value>,
        width: usize,
    ) -> Expr {
        let val = kids.iter().rev().find(|k| is_expr_node(k.kind()));
        match val {
            Some(v) => {
                let inner = self.lower_expr(v, env);
                let iw = match &inner {
                    Expr::Lit(v) => v.width().max(1),
                    _ => 1,
                };
                Expr::Replicate {
                    count: Box::new(Expr::Lit(Value::from_u64(32, (width / iw).max(1) as u64))),
                    e: Box::new(inner),
                }
            }
            None => Expr::Lit(Value::x(width.max(1))),
        }
    }

    /// Lower an aggregate against a record type `tn`, recursing into
    /// aggregate-typed fields with their own types (nested record/array
    /// aggregates keep their layout instead of degrading to X). Falls back
    /// to plain `lower_expr` per field when a field is not itself an
    /// aggregate of an aggregate type — so behavior matches
    /// [`Self::lower_record_aggregate`] for scalar fields.
    pub(super) fn typed_record_aggregate(
        &mut self,
        e: &SyntaxNode,
        tn: &str,
        fields: &[(String, usize, usize)],
        env: &HashMap<String, Value>,
    ) -> Expr {
        let kids = real_children(e);
        self.record_agg_kids(&kids, tn, fields, env)
    }

    /// Record-aggregate fold over an explicit element-child list — shared by
    /// [`Self::typed_record_aggregate`] (an `Aggregate` node whose
    /// children are the elements) and the qualified-record path (the parser
    /// folds `T'(field => v, …)` directly into the `Qualified` node, so the
    /// element children must be pre-extracted).
    pub(super) fn record_agg_kids(
        &mut self,
        kids: &[SyntaxNode],
        tn: &str,
        fields: &[(String, usize, usize)],
        env: &HashMap<String, Value>,
    ) -> Expr {
        let (named, positional) = self.record_agg_scan(kids);
        let others = named.iter().find(|(n, _)| n == "others").map(|(_, v)| *v);
        let mut parts: Vec<Expr> = Vec::new();
        for (fi, (fname, _, fw)) in fields.iter().enumerate() {
            let vi = named
                .iter()
                .find(|(n, _)| n == fname)
                .map(|(_, v)| *v)
                .or(others);
            let raw = match vi {
                Some(vi) => {
                    let vnode = &kids[vi];
                    let fty = self
                        .rec_field_ty
                        .get(&(tn.to_string(), fname.clone()))
                        .cloned();
                    self.lower_typed_field(vnode, fty.as_deref(), *fw, env)
                }
                None => positional
                    .get(fi)
                    .map(|&pi| self.lower_expr(&kids[pi], env))
                    .unwrap_or_else(|| Expr::Lit(Value::x(*fw))),
            };
            parts.push(Expr::Resize {
                e: Box::new(raw),
                width: *fw,
                signed: false,
            });
        }
        Expr::Concat(parts)
    }

    /// Split record-aggregate elements into named `field => value`
    /// associations (field name lowercased, value index) and positional
    /// value indexes, in source order.
    fn record_agg_scan(&self, kids: &[SyntaxNode]) -> (Vec<(String, usize)>, Vec<usize>) {
        let mut named: Vec<(String, usize)> = Vec::new();
        let mut positional: Vec<usize> = Vec::new();
        let mut i = 0;
        while i < kids.len() {
            let k = &kids[i];
            if self.is_assoc_arrow(kids, i) {
                named.push((self.text(k).to_ascii_lowercase(), i + 3));
                i += 4;
            } else {
                if is_expr_node(k.kind()) {
                    positional.push(i);
                }
                i += 1;
            }
        }
        (named, positional)
    }

    /// Lower one aggregate element/field value against its type: nested
    /// aggregates of record/array types recurse (typed layout); everything
    /// else lowers normally. `fw` is the target width for the fallback
    /// X-literal. `constraint` (decl-site range) supplies the array length
    /// and low bound when the value is an array aggregate.
    pub(super) fn lower_typed_field(
        &mut self,
        vnode: &SyntaxNode,
        fty: Option<&str>,
        fw: usize,
        env: &HashMap<String, Value>,
    ) -> Expr {
        if vnode.kind() != SyntaxKind::Aggregate {
            return self.lower_expr(vnode, env);
        }
        let Some(tn) = fty else {
            return self.lower_expr(vnode, env);
        };
        match self.types.get(tn).cloned() {
            Some(TypeInfo::Record { .. }) => {
                let fields = self.record_fields_of_type(tn);
                if fields.is_empty() {
                    return self.lower_expr(vnode, env);
                }
                self.typed_record_aggregate(vnode, tn, &fields, env)
            }
            Some(TypeInfo::Array { elem, .. }) => {
                let (n, lo) = self.array_agg_len(fw, elem);
                match n {
                    Some(n) => self
                        .lower_array_aggregate(vnode, tn, n, lo, env)
                        .unwrap_or_else(|| self.lower_expr(vnode, env)),
                    None => self.lower_expr(vnode, env),
                }
            }
            _ => self.lower_expr(vnode, env),
        }
    }

    pub(super) fn array_aggregate_specs(
        &mut self,
        e: &SyntaxNode,
        n: usize,
        lo: i64,
        elem: usize,
        elem_name: Option<String>,
        env: &HashMap<String, Value>,
    ) -> Option<Expr> {
        let elem = elem.max(1);
        let kids = real_children(e);
        let scan = self.array_agg_scan(&kids, env)?;
        let others = scan.others;
        let slots = self.array_agg_slots(scan, n, lo)?;
        let mut parts: Vec<Expr> = Vec::with_capacity(n);
        for slot in slots.iter().copied() {
            let raw = match slot.or(others) {
                Some(vi) => {
                    let vnode = kids[vi].clone();
                    self.lower_typed_field(&vnode, elem_name.as_deref(), elem, env)
                }
                None => Expr::Lit(Value::x(elem)),
            };
            parts.push(Expr::Resize {
                e: Box::new(raw),
                width: elem,
                signed: false,
            });
        }
        Some(Expr::Concat(parts))
    }

    /// Classify array-aggregate elements: named index choices (constant-
    /// folded; `None` on an unresolvable choice), positional values, and
    /// the `others` value index.
    fn array_agg_scan(
        &mut self,
        kids: &[SyntaxNode],
        env: &HashMap<String, Value>,
    ) -> Option<AggScan> {
        let mut named: Vec<(i64, usize)> = Vec::new();
        let mut positional: Vec<usize> = Vec::new();
        let mut others: Option<usize> = None;
        let mut i = 0;
        while i < kids.len() {
            let k = &kids[i];
            if self.is_assoc_arrow(kids, i) {
                if k.kind() == SyntaxKind::Others {
                    others = Some(i + 3);
                } else if is_expr_node(k.kind()) {
                    let cex = self.lower_expr(k, env);
                    let iv = self
                        .ce_env(&cex, env)
                        .and_then(|v| v.to_u64())
                        .map(|v| v as i64)?;
                    named.push((iv, i + 3));
                }
                i += 4;
            } else {
                if is_expr_node(k.kind()) {
                    positional.push(i);
                }
                i += 1;
            }
        }
        Some(AggScan {
            named,
            positional,
            others,
        })
    }

    /// Map choices onto element slots (`n` elements from low bound `lo`).
    /// A choice outside the constraint (or too many positionals) takes the
    /// legacy path: `None`.
    fn array_agg_slots(&self, scan: AggScan, n: usize, lo: i64) -> Option<Vec<Option<usize>>> {
        let mut slots: Vec<Option<usize>> = vec![None; n];
        for (iv, vi) in scan.named {
            let p = iv - lo;
            if p < 0 || p as usize >= n {
                return None; // choice outside the constraint: legacy path
            }
            slots[p as usize] = Some(vi);
        }
        for (j, &pi) in scan.positional.iter().enumerate() {
            if j >= n {
                return None;
            }
            slots[j] = Some(pi);
        }
        Some(slots)
    }
}
