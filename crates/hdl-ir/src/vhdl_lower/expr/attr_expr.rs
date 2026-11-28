//! Attribute and type-mark expression forms: `name'attr` (dim folds on
//! objects/signals/fields/aliases, event edges, placeholders), qualified
//! expressions `type_mark'(expr)` (conversion vs. record-aggregate
//! construction), and attribute-callee call forms (`A'length(1)`,
//! `integer'image(x)`, `type'val(x)`). Split out of `expr.rs` to keep every
//! function at its standards baseline.

use super::*;

impl<'a> Lower<'a> {
    /// Qualified expression `type_mark'(expr)` — a type conversion:
    /// lower the operand (the target type only matters for width, which
    /// the operand already carries).
    pub(super) fn lower_qualified(&mut self, e: &SyntaxNode, env: &HashMap<String, Value>) -> Expr {
        let kids = real_children(e);
        let rest = &kids[1..];
        // A qualified *aggregate* (`T'(field => v, others => x)`) is a
        // construction, not a conversion. The parser folds the
        // parenthesized element list directly into this node (no inner
        // Aggregate), so the detection is an element-association arrow
        // (`=` immediately followed by `>`) — a lone `=` (comparison
        // inside `boolean'(a = b)`) is not an aggregate.
        let is_aggregate = kids.windows(3).any(|w| {
            w[1].kind() == SyntaxKind::Punct
                && self.text(&w[1]) == "="
                && w[2].kind() == SyntaxKind::Punct
                && self.text(&w[2]) == ">"
        }) || rest.iter().any(|k| k.kind() == SyntaxKind::Others);
        if is_aggregate {
            return self.qualified_record_agg(&kids, env);
        }
        match rest.iter().find(|k| is_expr_node(k.kind())) {
            Some(k) => {
                // `string'("-")` — the qualifier forces the operand's
                // type: string-typed marks lower alphabet characters
                // as ASCII text, not 1-bit std_logic values.
                let saved = self.str_ctx;
                if kids.first().is_some_and(|tm| self.string_typed(tm)) {
                    self.str_ctx = true;
                }
                let ex = self.lower_expr(k, env);
                self.str_ctx = saved;
                ex
            }
            None => Expr::Lit(Value::x(32)),
        }
    }

    /// A qualified record aggregate folds like a typed record
    /// constant: resolve the type mark, skip the type mark and
    /// the paren puncts, and run the shared field scan. Array-
    /// qualified aggregates (`T'(1, 2)`, `s'(1 to N => ' ')`)
    /// keep the placeholder — unconstrained array length needs
    /// the decl constraint this node does not carry.
    fn qualified_record_agg(&mut self, kids: &[SyntaxNode], env: &HashMap<String, Value>) -> Expr {
        let tn = kids.first().map(|k| self.kw(k)).unwrap_or_default();
        if let Some(TypeInfo::Record { .. }) = self.types.get(&tn).cloned() {
            let fields = self.record_fields_of_type(&tn);
            let start = kids
                .iter()
                .position(|k| k.kind() == SyntaxKind::Punct && self.text(k) == "(")
                .map(|p| p + 1)
                .unwrap_or(1);
            let end = kids
                .last()
                .filter(|k| k.kind() == SyntaxKind::Punct && self.text(k) == ")")
                .map(|_| kids.len() - 1)
                .unwrap_or(kids.len());
            if !fields.is_empty() && start < end {
                return self.record_agg_kids(&kids[start..end], &tn, &fields, env);
            }
        }
        Expr::Lit(Value::x(32))
    }

    /// `name'attr` — only 'length/'high/'low/'range supported statically.
    pub(super) fn lower_attr(&mut self, e: &SyntaxNode) -> Expr {
        let kids = real_children(e);
        let base = kids.iter().find(|k| k.kind() == SyntaxKind::Name);
        let attr = kids
            .iter()
            .rfind(|k| k.kind() == SyntaxKind::Ident)
            .map(|k| self.kw(k));
        let Some(attr) = attr else {
            return Expr::Lit(Value::x(1));
        };
        let Some(b) = base else {
            return Expr::Lit(Value::x(1));
        };
        let bn = self.text(b);
        match attr.as_str() {
            "length" | "high" | "low" | "left" | "right" => self.obj_dim_attr(&bn, &attr),
            // `s'range` in expression position is only meaningful in a
            // for-spec (handled there); as a value it is X.
            "range" | "reverse_range" => Expr::Lit(Value::x(32)),
            // Report/simulation attributes: placeholder, no warning.
            "path_name" | "instance_name" | "last_event" | "image" | "reverse_image"
            | "encoding" | "max_wire_delay" | "subtype" | "simple_name" | "driving"
            | "driving_value" => Expr::Lit(Value::x(32)),
            // `sig'quiet` = no event this delta; `sig'active` = event.
            "quiet" => self.quiet_attr(&bn),
            "active" | "transaction" => self.edge_attr(&bn),
            "ascending" => Expr::Lit(Value::from_u64(1, 1)),
            // `sig'last_value` — value before the current delta.
            "last_value" => self.prev_attr(e, &bn, &attr),
            // `sig'stable` — no event this delta: NOT (rising OR falling).
            "stable" => self.stable_attr(e, &bn, &attr),
            // `sig'event` — true on any value change of sig this delta:
            // rising OR falling edge (scalar signals).
            "event" => self.event_attr(e, &bn, &attr),
            _ => {
                // User-declared attributes (`attribute a : T;`) have no
                // tracked value; X without a warning.
                if self.attr_decls.contains(&attr) {
                    return Expr::Lit(Value::x(32));
                }
                self.warn_attr(e, &bn, &attr)
            }
        }
    }

    /// `s'length` / `s'high` / `s'low` / `s'left` / `s'right` — fold from
    /// the object's recorded dims, the signal's shape, a record field's
    /// offset, a field/slice alias, or finally the type mark.
    fn obj_dim_attr(&mut self, bn: &str, attr: &str) -> Expr {
        // Constants/generics: the object's recorded dims (decl
        // constraint or initializer shape) fold the attribute
        // even when the type mark is unconstrained. Checked
        // before the signal path: generic names also sit in
        // the signal table carrying only their packed slot
        // width, which would fold `s'length` of a string
        // generic to 1.
        if let Some(ex) = self.obj_attr_expr(bn, attr) {
            return ex;
        }
        // Statically known for signals: arrays → element count
        // (mem depth), vectors → bit width.
        if let Some(id) = self.sig_id(bn) {
            return self.sig_dim_attr(id, attr);
        }
        // `sig.field[.field...]'length` — record field select on a
        // record-typed signal/port or record alias: the field's
        // packed width (first field = MSB).
        if let Some((b0, rest)) = bn.split_once('.') {
            if let Some(ex) = self.record_field_attr(b0, rest, attr) {
                return ex;
            }
        }
        // `alias'length` where the alias targets a field/slice
        // select with literal bounds.
        if let Some(ex) = self.alias_sel_attr(bn, attr) {
            return ex;
        }
        // Constants/generics: the object's recorded dims
        // (decl constraint or initializer shape) fold the
        // attribute even when the type mark is unconstrained.
        if let Some(ex) = self.obj_attr_expr(bn, attr) {
            return ex;
        }
        // Type marks and subtypes: `integer'high`, `my_arr'length`, ...
        self.type_attr(bn, attr)
    }

    /// Dimension attribute on a signal: unconstrained ports carry the
    /// attribute to runtime; formals and not-yet-sized signals evaluate
    /// the bound value's width; everything else folds statically.
    fn sig_dim_attr(&self, id: SigId, attr: &str) -> Expr {
        if self.uncon_ports.contains(&id) {
            // Width known only after port connection.
            return Expr::SigAttr {
                sig: id,
                attr: sig_attr_name(attr),
            };
        }
        let dims = self.sig_dims.get(&id);
        let mem_w = self.mems.get(&id).copied();
        let sig_w = self.sig_width.get(&id).copied();
        // Subprogram formals are sized by the call's
        // actuals; a stale `sig_width`/`sig_dims` entry
        // left by a previously lowered subprogram under
        // the same slot id must not bake a constant.
        if self.fn_formals.contains(&id) || (dims.is_none() && mem_w.is_none() && sig_w.is_none()) {
            return self.unsized_sig_attr(id, attr);
        }
        // Arrays: element count = product of dimension sizes
        // (sig_dims when present, else mem depth); vectors: bit
        // width. `low` is the first dimension's left.
        let n: usize = dims
            .and_then(|d| {
                if d.is_empty() {
                    None
                } else {
                    Some(d.iter().map(|(_, s)| (*s).max(1)).product::<usize>())
                }
            })
            .or(mem_w)
            .or(sig_w)
            .unwrap_or(1);
        let lo: i64 = dims.and_then(|d| d.first().map(|(l, _)| *l)).unwrap_or(0);
        let v = match attr {
            "length" => n as u64,
            "high" | "left" => (lo + n as i64 - 1).max(0) as u64,
            _ => lo.max(0) as u64, // low | right
        };
        Expr::Lit(Value::from_u64(32, v))
    }

    /// Function formals and other not-yet-sized
    /// signals: the width is only known from the
    /// bound value at runtime (`arg'length` inside
    /// an unconstrained-function body). A baked
    /// constant truncated
    /// `conv_unsigned(count, arg'length)` to a
    /// 1-bit shift count. `length` is an element
    /// count, but the runtime and const-eval both
    /// evaluate `SigAttr` as the bound value's bit
    /// width — divide by the element width
    /// (`string`: 8) right here, where the element
    /// width is known.
    fn unsized_sig_attr(&self, id: SigId, attr: &str) -> Expr {
        let sa = Expr::SigAttr {
            sig: id,
            attr: sig_attr_name(attr),
        };
        if attr == "length" {
            let ew = self.sig_elem.get(&id).copied().unwrap_or(1);
            if ew > 1 {
                return Expr::Bin {
                    op: BinOp::Div,
                    l: Box::new(sa),
                    r: Box::new(Expr::Lit(Value::from_u64(32, ew as u64))),
                };
            }
        }
        sa
    }

    /// `sig.field[.field...]'length` — record field select on a
    /// record-typed signal/port or record alias: the field's
    /// packed width (first field = MSB). `None` falls through.
    fn record_field_attr(&self, b0: &str, rest: &str, attr: &str) -> Option<Expr> {
        let path: Vec<&str> = rest.split('.').collect();
        let tn = self
            .sig_id(b0)
            .and_then(|i| self.sig_rec.get(&i).cloned())
            .or_else(|| self.alias_rec.get(b0).cloned())?;
        let (lsb, w) = self.record_field_offset(&tn, &path)?;
        let v = match attr {
            "length" => w as u64,
            "high" | "left" => (lsb + w - 1) as u64,
            _ => lsb as u64, // low | right
        };
        Some(Expr::Lit(Value::from_u64(32, v)))
    }

    /// `alias'length` where the alias targets a field/slice
    /// select with literal bounds. `None` falls through.
    fn alias_sel_attr(&self, bn: &str, attr: &str) -> Option<Expr> {
        let (msb, lsb) = match self.aliases.get(&bn.to_ascii_lowercase()) {
            Some(Expr::Sel { msb, lsb, .. }) => (msb, lsb),
            _ => return None,
        };
        if let (Some(m), Some(l)) = (self.ce(msb), self.ce(lsb)) {
            if let (Some(m), Some(l)) = (m.to_u64(), l.to_u64()) {
                let v = match attr {
                    "length" => (m as i64 - l as i64).unsigned_abs() + 1,
                    "high" | "left" => m,
                    _ => l, // low | right
                };
                return Some(Expr::Lit(Value::from_u64(32, v)));
            }
        }
        None
    }

    /// `sig'quiet` = no event this delta.
    fn quiet_attr(&self, bn: &str) -> Expr {
        if let Some(id) = self.sig_id(bn) {
            return Expr::Un {
                op: UnOp::Not,
                e: Box::new(self.edge_event_expr(id)),
            };
        }
        Expr::Lit(Value::x(1))
    }

    /// `sig'active` / `sig'transaction` = event this delta.
    fn edge_attr(&self, bn: &str) -> Expr {
        if let Some(id) = self.sig_id(bn) {
            return self.edge_event_expr(id);
        }
        Expr::Lit(Value::x(1))
    }

    /// `sig'last_value` — value before the current delta.
    fn prev_attr(&mut self, e: &SyntaxNode, bn: &str, attr: &str) -> Expr {
        if let Some(id) = self.sig_id(bn) {
            return Expr::Prev(Box::new(Expr::Sig(id)));
        }
        self.warn_attr(e, bn, attr)
    }

    /// `sig'stable` — no event this delta: NOT (rising OR falling).
    fn stable_attr(&mut self, e: &SyntaxNode, bn: &str, attr: &str) -> Expr {
        if let Some(id) = self.sig_id(bn) {
            return Expr::Un {
                op: UnOp::Not,
                e: Box::new(self.edge_event_expr(id)),
            };
        }
        self.warn_attr(e, bn, attr)
    }

    /// `sig'event` — true on any value change of sig this delta:
    /// rising OR falling edge (scalar signals).
    fn event_attr(&mut self, e: &SyntaxNode, bn: &str, attr: &str) -> Expr {
        if let Some(id) = self.sig_id(bn) {
            return self.edge_event_expr(id);
        }
        self.warn_attr(e, bn, attr)
    }

    /// Rising OR falling edge of a scalar signal this delta.
    fn edge_event_expr(&self, id: SigId) -> Expr {
        let s = || Box::new(Expr::Sig(id));
        Expr::Bin {
            op: BinOp::LogOr,
            l: Box::new(Expr::Rising(s())),
            r: Box::new(Expr::Falling(s())),
        }
    }

    /// Unsupported attribute on an unknown base: warn, X.
    fn warn_attr(&mut self, e: &SyntaxNode, bn: &str, attr: &str) -> Expr {
        self.warn(
            e.span(),
            format!("attribute '{}\\'{}' not supported; X", bn, attr),
        );
        Expr::Lit(Value::x(1))
    }

    /// Call with an attribute callee: `integer'image(x)` arrives as
    /// Call(Attr(Name integer, 'image), args). `None` keeps the plain
    /// callee path below.
    pub(super) fn lower_attr_callee(
        &mut self,
        e: &SyntaxNode,
        env: &HashMap<String, Value>,
        kids: &[SyntaxNode],
        attr_pos: Option<usize>,
    ) -> Option<Expr> {
        let ap = attr_pos?;
        let ak = real_children(&kids[ap]);
        let attr_name = ak
            .iter()
            .rfind(|k| k.kind() == SyntaxKind::Ident)
            .map(|k| self.kw(k))
            .unwrap_or_default();
        // Type mark the attribute is applied to (`boolean'image`,
        // `A'length(1)`'s base, …).
        let prefix = ak
            .iter()
            .find(|k| k.kind() == SyntaxKind::Name)
            .map(|k| self.kw(k))
            .unwrap_or_default();
        if matches!(
            attr_name.as_str(),
            "length" | "high" | "low" | "left" | "right" | "ascending"
        ) {
            return self.attr_dim_call(kids, ap, &ak, env, &attr_name);
        }
        if attr_name == "image" {
            if let Some(arg) = kids
                .iter()
                .enumerate()
                .find(|(i, k)| *i != ap && is_expr_node(k.kind()))
                .map(|(_, k)| k)
            {
                return Some(self.attr_image_expr(arg, env, &prefix));
            }
        }
        // `type'val(x)` / `type'pos(x)` — numeric identity, but 'val takes
        // the prefix type's width (character'val(65) is 8 bits). Only take
        // this path when the argument resolves in the current scope — a
        // bare name that does not (e.g. an entity-declared enum literal
        // used in an architecture) keeps the old call fallback.
        if attr_name == "val" || attr_name == "pos" {
            return self.attr_val_pos_call(e, env, kids, ap, &ak, &attr_name);
        }
        None
    }

    /// Array attributes with an explicit dimension — `A'length(1)`,
    /// `A'high(2)` (LRM 1076 §5.2.1) arrive as Call(Attr(prefix, attr),
    /// [dim]). Fold when the dimension's bounds are statically known
    /// (signals, constants/generics with known dims, array types and
    /// subtypes); otherwise X.
    fn attr_dim_call(
        &mut self,
        kids: &[SyntaxNode],
        ap: usize,
        ak: &[SyntaxNode],
        env: &HashMap<String, Value>,
        attr_name: &str,
    ) -> Option<Expr> {
        let arg = kids
            .iter()
            .enumerate()
            .find(|(i, k)| *i != ap && is_expr_node(k.kind()))
            .map(|(_, k)| k)?;
        let dim_e = self.lower_expr(arg, env);
        let dim = self.ce(&dim_e).and_then(|v| v.to_u64()).unwrap_or(1).max(1) as usize;
        let base = ak
            .iter()
            .find(|k| k.kind() == SyntaxKind::Name)
            .map(|k| self.kw(k))
            .unwrap_or_default();
        if let Some(v) = self.attr_dim_value(&base, attr_name, dim) {
            if attr_name == "ascending" {
                return Some(Expr::Lit(Value::from_u64(1, v)));
            }
            return Some(Expr::Lit(Value::from_u64(32, v)));
        }
        Some(Expr::Lit(Value::x(if attr_name == "ascending" {
            1
        } else {
            32
        })))
    }

    /// `type'val(x)` / `type'pos(x)` emit a `$val`/`$pos` call; 'val
    /// additionally resizes to the prefix type's width.
    fn attr_val_pos_call(
        &mut self,
        e: &SyntaxNode,
        env: &HashMap<String, Value>,
        kids: &[SyntaxNode],
        ap: usize,
        ak: &[SyntaxNode],
        attr_name: &str,
    ) -> Option<Expr> {
        let arg = kids
            .iter()
            .enumerate()
            .find(|(i, k)| *i != ap && is_expr_node(k.kind()))
            .map(|(_, k)| k);
        let resolvable = match arg {
            Some(a) if a.kind() == SyntaxKind::Name => {
                let k = self.kw(a).to_ascii_lowercase();
                env.contains_key(&k)
                    || self.consts.contains_key(&k)
                    || self.names.contains_key(&k)
                    || self.funcs.contains_key(&k)
                    || self.sig_id(&k).is_some()
            }
            Some(_) => true,
            None => false,
        };
        let arg = arg?;
        if !resolvable {
            return None;
        }
        let inner = Expr::Call {
            name: format!("${attr_name}"),
            args: vec![self.lower_expr(arg, env)],
            shapes: Vec::new(),
        };
        if attr_name == "val" {
            let prefix = ak.iter().find(|k| k.kind() == SyntaxKind::Name).cloned();
            if let Some(pn) = prefix {
                let (w, _, _, _) = self.resolve_type(&pn, None, e.span());
                return Some(Expr::Resize {
                    e: Box::new(inner),
                    width: w.max(1),
                    signed: false,
                });
            }
        }
        Some(inner)
    }
}

/// The five foldable signal attribute names pass through unchanged (the
/// arm guard already restricts `attr` to these).
fn sig_attr_name(attr: &str) -> &'static str {
    match attr {
        "length" => "length",
        "high" => "high",
        "low" => "low",
        "left" => "left",
        _ => "right",
    }
}
