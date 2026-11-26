//! Aggregate and RHS lowering: array/record aggregates, conditional waveforms, statement-level RHS lowering, and integer literal parsing.
//!
//! Extracted verbatim from vhdl_lower.rs (pure move, no logic changes).

use super::*;

impl<'a> Lower<'a> {
    /// Lower an array aggregate against element type `tn` with `n` elements
    /// flat-indexed from `lo`: named index choices (`0 => v`) place by
    /// value, `others` fills the rest, positional elements fill in order.
    /// Every element is resized to the element width; the result concatenates
    /// element 0 first (flat convention: element 0 = MSB).
    pub(super) fn lower_array_aggregate(
        &mut self,
        e: &SyntaxNode,
        tn: &str,
        n: usize,
        lo: i64,
        env: &HashMap<String, Value>,
    ) -> Option<Expr> {
        let Some(TypeInfo::Array {
            elem, elem_name, ..
        }) = self.types.get(tn).cloned()
        else {
            return None;
        };
        self.lower_array_aggregate_specs(e, n, lo, elem, elem_name, env)
    }

    /// Aggregate assigned to an array-typed signal: same element-ordinal
    /// layout as [`Self::lower_array_aggregate`] (element `lo` first = most
    /// significant), driven by the signal's declared dims/depth instead of
    /// a type name.
    pub(super) fn lower_array_aggregate_for_sig(
        &mut self,
        e: &SyntaxNode,
        id: SigId,
        env: &HashMap<String, Value>,
    ) -> Option<Expr> {
        let (lo, n) = match self.sig_dims.get(&id) {
            Some(d) if !d.is_empty() => (d[0].0, d[0].1.max(1)),
            _ => (0, (*self.mems.get(&id)?).max(1)),
        };
        // `sig_width` is the element width for memory-backed signals, the
        // flat width otherwise — divide out the element count there.
        let ew = if self.mems.contains_key(&id) {
            (*self.sig_width.get(&id)?).max(1)
        } else {
            (*self.sig_width.get(&id)?).max(1).saturating_div(n)
        };
        let elem_rec = self.sig_elem_rec.get(&id).cloned();
        self.lower_array_aggregate_specs(e, n, lo, ew.max(1), elem_rec, env)
    }

    /// Field layout of a record type name: (field name, lsb offset, width)
    /// per field, in declaration order (first field = MSB of the flat vector).
    pub(super) fn record_fields_of_type(&self, type_name: &str) -> Vec<(String, usize, usize)> {
        let Some(TypeInfo::Record { fields }) = self.types.get(type_name) else {
            return Vec::new();
        };
        let total: usize = fields.iter().map(|f| f.1).sum();
        let mut run = 0usize;
        let mut out = Vec::new();
        for (fn_, w, _) in fields {
            out.push((fn_.clone(), total - run - w, *w));
            run += w;
        }
        out
    }

    /// Field layout of a record-typed LHS node, if it is one.
    pub(super) fn record_fields_of_lhs(
        &self,
        lhs: &SyntaxNode,
    ) -> Option<Vec<(String, usize, usize)>> {
        if lhs.kind() != SyntaxKind::Name {
            return None;
        }
        let name = self.text(lhs);
        let base = name.split('.').next().unwrap_or("").to_ascii_lowercase();
        let id = self.sig_id(&base)?;
        let tn = self.sig_rec.get(&id)?;
        let out = self.record_fields_of_type(tn);
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    }

    /// Lower a record aggregate: named associations (`field => value`) are
    /// placed by declaration order, positional ones in order; each field value
    /// is resized to the field width. Unassigned fields (no `others`) → X.
    pub(super) fn lower_record_aggregate(
        &mut self,
        e: &SyntaxNode,
        fields: &[(String, usize, usize)],
        env: &HashMap<String, Value>,
    ) -> Expr {
        let kids = real_children(e);
        let mut named: Vec<(String, usize)> = Vec::new();
        let mut positional: Vec<usize> = Vec::new();
        let mut i = 0;
        while i < kids.len() {
            let k = &kids[i];
            let arrow = kids
                .get(i + 1)
                .map(|p| p.kind() == SyntaxKind::Punct && self.text(p) == "=")
                .unwrap_or(false)
                && kids
                    .get(i + 2)
                    .map(|p| p.kind() == SyntaxKind::Punct && self.text(p) == ">")
                    .unwrap_or(false);
            if arrow {
                named.push((self.text(k).to_ascii_lowercase(), i + 3));
                i += 4;
            } else if is_expr_node(k.kind()) {
                positional.push(i);
                i += 1;
            } else {
                i += 1;
            }
        }
        let others = named.iter().find(|(n, _)| n == "others").map(|(_, v)| *v);
        let mut parts: Vec<Expr> = Vec::new();
        for (fi, (fname, _, fw)) in fields.iter().enumerate() {
            let vi = named
                .iter()
                .find(|(n, _)| n == fname)
                .map(|(_, v)| *v)
                .or(others);
            let raw = match vi {
                Some(vi) => self.lower_expr(&kids[vi], env),
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

    /// Lower an assignment RHS, using the LHS record type (if any) to order
    /// named aggregate associations.
    /// Lower a flat conditional waveform / expression
    /// (`v0 when c0 else v1 when c1 else v2`) into nested `Expr::Cond`.
    pub(super) fn lower_cond_waveform(
        &mut self,
        kids: &[SyntaxNode],
        env: &HashMap<String, Value>,
    ) -> Option<Expr> {
        let mut pairs: Vec<(Expr, Option<Expr>)> = Vec::new();
        let mut cur_val: Option<Expr> = None;
        let mut cur_cond: Option<Expr> = None;
        let mut i = 0;
        while i < kids.len() {
            let k = &kids[i];
            if k.kind() == SyntaxKind::Ident {
                match self.kw(k).as_str() {
                    "when" => {
                        if let Some(c) = kids.get(i + 1).filter(|e| is_expr_node(e.kind())) {
                            cur_cond = Some(self.lower_expr(c, env));
                            i += 2;
                            continue;
                        }
                    }
                    "else" => {
                        if let Some(v) = cur_val.take() {
                            pairs.push((v, cur_cond.take()));
                        }
                        if let Some(e) = kids.get(i + 1).filter(|e| is_expr_node(e.kind())) {
                            cur_val = Some(self.lower_expr(e, env));
                            i += 2;
                            continue;
                        }
                    }
                    _ => {}
                }
            } else if is_expr_node(k.kind()) {
                if let Some(v) = cur_val.take() {
                    pairs.push((v, cur_cond.take()));
                }
                cur_val = Some(self.lower_expr(k, env));
            }
            i += 1;
        }
        if let Some(v) = cur_val {
            pairs.push((v, cur_cond));
        }
        let mut acc: Option<Expr> = None;
        for (v, c) in pairs.into_iter().rev() {
            acc = Some(match c {
                Some(c) => Expr::Cond {
                    c: Box::new(c),
                    t: Box::new(v),
                    e: Box::new(acc.unwrap_or(Expr::Lit(Value::x(1)))),
                },
                None => v,
            });
        }
        acc
    }

    pub(super) fn lower_rhs(
        &mut self,
        lhs: &SyntaxNode,
        rhs: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) -> Expr {
        if rhs.kind() == SyntaxKind::Aggregate {
            if let Some(fields) = self.record_fields_of_lhs(lhs) {
                return self.lower_record_aggregate(rhs, &fields, env);
            }
            // Array-typed target: named choices (`0 => '1', others => '0`)
            // must place their elements — the width-only fallback keeps
            // only `others` and turns the rest into the others-value.
            if let Some(id) = self.array_lhs_sig(lhs) {
                if let Some(ex) = self.lower_array_aggregate_for_sig(rhs, id, env) {
                    return ex;
                }
            }
        }
        // A plain OTHERS aggregate into a string target replicates over the
        // target's whole bit width (`(others => ' ')` filled only 4 of 5
        // characters with the baked 32-bit fallback — see `agg_target_w`).
        let saved_w = self.agg_target_w;
        if rhs.kind() == SyntaxKind::Aggregate {
            if let Some(id) = self.sig_id(&self.text(lhs)) {
                if self.sig_elem.get(&id) == Some(&8) {
                    if let Some(w) = self.sig_width.get(&id) {
                        self.agg_target_w = Some(*w);
                    }
                }
            }
        }
        let ex = self.lower_expr(rhs, env);
        self.agg_target_w = saved_w;
        ex
    }

    /// The signal id behind an assignment target whose type is an array
    /// (declared dims or memory-backed) — the anchor for typed aggregate
    /// lowering of the RHS.
    pub(super) fn array_lhs_sig(&self, lhs: &SyntaxNode) -> Option<SigId> {
        if lhs.kind() != SyntaxKind::Name {
            return None;
        }
        let name = self.text(lhs);
        let base = name.split('.').next().unwrap_or("").to_ascii_lowercase();
        let id = self.sig_id(&base)?;
        if self.sig_dims.contains_key(&id) || self.mems.contains_key(&id) {
            Some(id)
        } else {
            None
        }
    }

    /// Parse a VHDL integer literal: decimal (`1_000`) or based (`16#FF#E1`).
    pub(super) fn parse_int_literal(&mut self, t: &str) -> Option<Value> {
        let t = t.trim();
        if let Some(h) = t.find('#') {
            let base: u32 = t[..h].parse().ok()?;
            if !(2..=36).contains(&base) {
                return None; // invalid base (`0#...#`) must not panic
            }
            let rest = &t[h + 1..];
            let end = rest.find('#')?;
            let digits = rest[..end].replace('_', "");
            let exp = rest[end + 1..]
                .strip_prefix(['e', 'E'])
                .and_then(|e| e.parse::<i32>().ok())
                .unwrap_or(0);
            let mut v = u128::from_str_radix(&digits, base).ok()?;
            if exp > 0 {
                v = v.checked_mul((base as u128).checked_pow(exp as u32)?)?;
            }
            let width = (128 - v.leading_zeros() as usize).max(1);
            return Some(value_from_u128(width, v));
        }
        let digits = t.replace('_', "");
        let v = digits.parse::<u128>().ok()?;
        // Plain decimal literals are VHDL integers: 32-bit, so `i + 1` does
        // not sign-extend the 1-bit literal `1` (based literals like 16#FF#
        // keep their minimal width and are usually bit patterns).
        Some(value_from_u128(32, v))
    }
}
