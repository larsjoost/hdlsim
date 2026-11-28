//! VHDL expression lowering: expressions, calls, attribute access,
//! literals, and aggregates. Extracted verbatim from the lowerer body
//! (pure move, no logic changes); decomposition into per-kind helpers
//! follows in separate changes.

use super::*;

impl<'a> Lower<'a> {
    /// A folded constant/generic value at a use site: a bare literal loses
    /// the object's type, so a signed integer is wrapped in `Signed` to keep
    /// ordered comparisons signed (`G_LEN >= 0` with `G_LEN = -1` would
    /// otherwise read as 4294967295 >= 0).
    fn const_use(&self, key: &str, v: Value) -> Expr {
        let e = Expr::Lit(v);
        if let Some(tn) = self.const_types.get(key) {
            if matches!(self.types.get(tn), Some(TypeInfo::Integer { signed: true })) {
                return Expr::Signed(Box::new(e));
            }
        }
        e
    }

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
            // Qualified expression `type_mark'(expr)` — a type conversion:
            // lower the operand (the target type only matters for width, which
            // the operand already carries).
            SyntaxKind::Qualified => {
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
                    // A qualified record aggregate folds like a typed record
                    // constant: resolve the type mark, skip the type mark and
                    // the paren puncts, and run the shared field scan. Array-
                    // qualified aggregates (`T'(1, 2)`, `s'(1 to N => ' ')`)
                    // keep the placeholder — unconstrained array length needs
                    // the decl constraint this node does not carry.
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
                            return self.lower_record_agg_kids(
                                &kids[start..end],
                                &tn,
                                &fields,
                                env,
                            );
                        }
                    }
                    return Expr::Lit(Value::x(32));
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
            SyntaxKind::Name => {
                let text = self.text(e);
                // VHDL-2008 `.all` dereference of an access object: lower the
                // prefix (`Buf.all` -> `Buf`).
                let stripped = if text.len() > 4
                    && text.as_bytes()[text.len() - 4] == b'.'
                    && text[text.len() - 3..].eq_ignore_ascii_case("all")
                {
                    text[..text.len() - 4].to_string()
                } else {
                    text
                };
                // Mid-chain `.all` dereference: `res.all.chain` -> `res.chain`.
                let name = stripped
                    .split('.')
                    .filter(|s| !s.eq_ignore_ascii_case("all"))
                    .collect::<Vec<_>>()
                    .join(".");
                // `obj.m` / `obj.field` on a protected-type object in
                // expression position (parameterless method call / field).
                // Unknown members (and package-level shared variables, which
                // have no per-module object yet) stay placeholders.
                if let Some(dot) = name.rfind('.') {
                    let (base, last) = (&name[..dot], name[dot + 1..].to_ascii_lowercase());
                    let base_lc = base.to_ascii_lowercase();
                    // Subprogram/process locals live in names_stack, not names.
                    let id = self.sig_id(base).or_else(|| {
                        self.names_stack.iter().rev().find_map(|m| {
                            m.get(&base_lc).copied().or_else(|| {
                                m.iter()
                                    .find(|(k, _)| k.eq_ignore_ascii_case(&base_lc))
                                    .map(|(_, v)| *v)
                            })
                        })
                    });
                    if let Some(id) = id {
                        if let Some(t) = self.sig_prot.get(&id).cloned() {
                            if let Some(pt) = self.prot_types.get(&t) {
                                if pt.methods.contains(&last) {
                                    return Expr::Call {
                                        name: format!("$method::{}::{}", t, last),
                                        args: vec![Expr::Sig(id)],
                                        shapes: Vec::new(),
                                    };
                                }
                                if let Some((_, w, lsb, _)) =
                                    pt.fields.iter().find(|(n, _, _, _)| *n == last)
                                {
                                    return Expr::Call {
                                        name: format!("$getf::{}::{}@{},{}", t, last, lsb, w),
                                        args: vec![Expr::Sig(id)],
                                        shapes: Vec::new(),
                                    };
                                }
                                // Unknown member on a known protected object.
                                return Expr::Lit(Value::x(32));
                            } else {
                                // Protected type from a use'd package (no
                                // local body): member select is a placeholder.
                                return Expr::Lit(Value::x(32));
                            }
                        }
                    } else if let Some(tn) = self.pkg_sig_types.get(base) {
                        if matches!(self.types.get(tn), Some(TypeInfo::Handle)) {
                            // Package-level shared variable of protected type:
                            // not simulated yet — placeholder.
                            return Expr::Lit(Value::x(32));
                        }
                    }
                }
                // Structural selected-name chain (`sig.f(i).g`, rooted at a
                // signal/constant/alias): walk field + index segments with
                // type tracking. Falls back to the per-shape handling below
                // when the chain does not resolve this way.
                if name.contains('.') {
                    if let Some(ex) = self.lower_selected_chain(e, env) {
                        return ex;
                    }
                }
                // `arr(i).field` — the parser folds this into one Name node
                // wrapping a Call plus `.field`. Lower the call, then select
                // the record field inside the element (slice of the memory
                // element for arrays of records).
                let ekids = real_children(e);
                if let Some(call) = ekids.iter().find(|k| k.kind() == SyntaxKind::Call).cloned() {
                    let field = ekids
                        .iter()
                        .rfind(|k| k.kind() == SyntaxKind::Ident)
                        .map(|k| self.kw(k))
                        .unwrap_or_default();
                    let base = self.lower_expr(&call, env);
                    // `v(i).all` (access auto-deref after indexing) or a bare
                    // folded `v(i)`: the index already applied — nothing more
                    // to select.
                    if field.is_empty() || field == "all" {
                        return base;
                    }
                    let rec_name = match &base {
                        Expr::Idx { e: be, .. } => match &**be {
                            Expr::Sig(id) => self
                                .sig_elem_rec
                                .get(id)
                                .or_else(|| self.sig_rec.get(id))
                                .cloned(),
                            _ => None,
                        },
                        Expr::Sig(id) => self
                            .sig_rec
                            .get(id)
                            .or_else(|| self.sig_elem_rec.get(id))
                            .cloned(),
                        // `const_arr(i).field` lowered via `$const_idx`: the
                        // element record type comes from the constant's type.
                        Expr::Call { name, .. } if name == "$const_idx" => {
                            let ck = real_children(&call);
                            let cn = ck
                                .iter()
                                .find(|k| k.kind() == SyntaxKind::Name)
                                .map(|k| self.text(k))
                                .unwrap_or_default();
                            let cn = cn
                                .rsplit('.')
                                .next()
                                .unwrap_or(cn.as_str())
                                .to_ascii_lowercase();
                            self.const_types
                                .get(&cn)
                                .and_then(|tn| match self.types.get(tn) {
                                    Some(TypeInfo::Array {
                                        elem_name: Some(en),
                                        ..
                                    }) => Some(en.clone()),
                                    _ => None,
                                })
                        }
                        _ => None,
                    };
                    if rec_name.is_none() && !field.is_empty() {
                        // Field select on an indexed/opaque record element
                        // (`state.actors(0).subscribers`) — placeholder.
                        return Expr::Lit(Value::x(32));
                    }
                    if let Some(tn) = rec_name {
                        if let Some(TypeInfo::Record { fields }) = self.types.get(&tn) {
                            let total: usize = fields.iter().map(|f| f.1).sum();
                            let mut run = 0usize;
                            for (fn_, w, _) in fields {
                                let lsb = total - run - w;
                                if *fn_ == field {
                                    return Expr::Sel {
                                        e: Box::new(base),
                                        msb: Box::new(Expr::Lit(Value::from_u64(
                                            32,
                                            (lsb + w - 1) as u64,
                                        ))),
                                        lsb: Box::new(Expr::Lit(Value::from_u64(32, lsb as u64))),
                                    };
                                }
                                run += w;
                            }
                        }
                    }
                }
                // Record field select: `r.field` (or nested `r.a.b`) on a
                // record-typed signal → slice of the flat record vector
                // (first field = MSB).
                if name.contains('.') {
                    let segs: Vec<&str> = name.split('.').collect();
                    let base = segs[0].to_ascii_lowercase();
                    let fields_path = &segs[1..];
                    let field = fields_path.join(".").to_ascii_lowercase();
                    let id = self
                        .sig_id(&base)
                        .or_else(|| self.names.get(&format!("__const_{}", base)).copied())
                        .or_else(|| {
                            self.names_stack.iter().rev().find_map(|m| {
                                m.get(&base).copied().or_else(|| {
                                    m.iter()
                                        .find(|(k, _)| k.eq_ignore_ascii_case(&base))
                                        .map(|(_, v)| *v)
                                })
                            })
                        });
                    if let Some(id) = id {
                        if let Some(tn) = self.sig_rec.get(&id).cloned() {
                            let path: Vec<String> =
                                fields_path.iter().map(|s| s.to_ascii_lowercase()).collect();
                            let path_refs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                            if let Some((lsb, w)) = self.record_field_offset(&tn, &path_refs) {
                                return Expr::Sel {
                                    e: Box::new(Expr::Sig(id)),
                                    msb: Box::new(Expr::Lit(Value::from_u64(
                                        32,
                                        (lsb + w - 1) as u64,
                                    ))),
                                    lsb: Box::new(Expr::Lit(Value::from_u64(32, lsb as u64))),
                                };
                            }
                        }
                    }
                    // Field select rooted at a record alias (`alias LAW : R'subtype
                    // is sig.field;` → `LAW.x` slices the aliased expression).
                    if id.is_none() {
                        if let (Some(ae), Some(tn)) = (
                            self.aliases.get(&base).cloned(),
                            self.alias_rec.get(&base).cloned(),
                        ) {
                            let path: Vec<String> =
                                fields_path.iter().map(|s| s.to_ascii_lowercase()).collect();
                            let path_refs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                            if let Some((lsb, w)) = self.record_field_offset(&tn, &path_refs) {
                                return Expr::Sel {
                                    e: Box::new(ae),
                                    msb: Box::new(Expr::Lit(Value::from_u64(
                                        32,
                                        (lsb + w - 1) as u64,
                                    ))),
                                    lsb: Box::new(Expr::Lit(Value::from_u64(32, lsb as u64))),
                                };
                            }
                        }
                    }
                    // Nested member select rooted at a protected-type state
                    // variable (`st.ptr`, `st` a record-typed state field):
                    // placeholder (not simulated).
                    if env.get(&base).is_none() {
                        if let Some(t) = self.prot_body_ctx.clone() {
                            if let Some(pt) = self.prot_types.get(&t) {
                                if pt.fields.iter().any(|(n, _, _, _)| *n == base) {
                                    return Expr::Lit(Value::x(32));
                                }
                            }
                        }
                    }
                    // Package-level signal / shared variable: record-field or
                    // protected-type member select lowers to a placeholder
                    // (not simulated).
                    if let Some(tn) = self.pkg_sig_types.get(&base).cloned() {
                        if let Some(TypeInfo::Record { fields }) = self.types.get(&tn) {
                            if let Some((_, w, _)) = fields.iter().find(|(fn_, _, _)| *fn_ == field)
                            {
                                return Expr::Lit(Value::x((*w).max(1)));
                            }
                            return Expr::Lit(Value::x(32));
                        }
                    }
                    if let (Some(v), Some(tn)) = (
                        self.consts.get(&base).cloned(),
                        self.const_types.get(&base).cloned(),
                    ) {
                        // `rec_const.field` (nested: `c.a.b`) on a record-typed
                        // constant/generic: slice the literal value directly.
                        let path: Vec<String> =
                            fields_path.iter().map(|s| s.to_ascii_lowercase()).collect();
                        let path_refs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                        if let Some((lsb, w)) = self.record_field_offset(&tn, &path_refs) {
                            return Expr::Sel {
                                e: Box::new(Expr::Lit(v)),
                                msb: Box::new(Expr::Lit(Value::from_u64(32, (lsb + w - 1) as u64))),
                                lsb: Box::new(Expr::Lit(Value::from_u64(32, lsb as u64))),
                            };
                        }
                    }
                }
                let base = name.rsplit('.').next().unwrap_or(name.as_str());
                let key = base.to_ascii_lowercase();
                // Local scope (env: loop vars, params) first, then signals, then
                // constants — so an enum literal from a `use`d package (e.g. 'U'
                // from std_logic_1164) cannot shadow a signal named `u`.
                // Inside a function body `names` holds exactly the
                // formals/locals, so a formal shadowing an outer constant that
                // reached the env snapshot wins (LRM 10.3 inner visibility):
                // an architecture constant `C_DATA` must not override a
                // formal `C_DATA` in the body.
                if let Some(v) = env.get(&key) {
                    // Inside a function body `names` holds exactly the
                    // formals/locals, so a formal shadowing an outer constant
                    // that reached the env snapshot wins (LRM 10.3 inner
                    // visibility): an architecture constant `C_DATA` must not
                    // override a formal `C_DATA` in the body. `sig_id` is
                    // case-insensitive; `names` keys keep the source case.
                    if self.fn_scope == 0 || self.sig_id(base).is_none() {
                        return self.const_use(&key, v.clone());
                    }
                }
                if let Some(id) = self.sig_id(base) {
                    return Expr::Sig(id);
                }
                if let Some(v) = self.consts.get(&key) {
                    return self.const_use(&key, v.clone());
                }
                if let Some(a) = self.aliases.get(&key) {
                    return a.clone();
                }
                // Function referenced by name (parameterless call `c_hi`, or an
                // alias target `alias a is f[...];`): resolve to a call. When
                // every formal carries a recorded default, the bare name is a
                // legal parameterless call — emit no actuals, the omitted
                // formals take their defaults. Otherwise placeholder actuals
                // keep the reference a call (parameterized bare names are only
                // legal as alias targets; they read X).
                if let Some(f) = self.funcs.get(&key).and_then(|c| c.first()) {
                    let n_actuals =
                        if !f.defaults.is_empty() && f.defaults.iter().all(|d| d.is_some()) {
                            0
                        } else {
                            f.args.len()
                        };
                    return Expr::Call {
                        name: f.name.clone(),
                        args: vec![Expr::Lit(Value::x(32)); n_actuals],
                        shapes: Vec::new(),
                    };
                }
                // `open` (reserved word) in a port/generic map association:
                // an unconnected port — X, no diagnostic.
                if key == "open" {
                    return Expr::Lit(Value::x(1));
                }
                // A type name used as a value (`alias t2 is t1;`) — placeholder.
                if self.types.contains_key(&key) {
                    return Expr::Lit(Value::x(32));
                }
                // std.standard `now` — current simulation time (sim: `$now`).
                if key == "now" {
                    return Expr::Call {
                        name: "$now".into(),
                        args: Vec::new(),
                        shapes: Vec::new(),
                    };
                }
                // Xilinx implicit globals (unisim references bare `GSR` etc.).
                if matches!(key.as_str(), "gsr" | "grestore" | "gts") {
                    return Expr::Sig(self.ensure_global_sig(base));
                }
                // Enclosing function scope (nested functions see outer
                // parameters/locals).
                if let Some(id) = self.names_stack.iter().rev().find_map(|m| {
                    m.get(&key).copied().or_else(|| {
                        m.iter()
                            .find(|(k, _)| k.eq_ignore_ascii_case(&key))
                            .map(|(_, v)| *v)
                    })
                }) {
                    return Expr::Sig(id);
                }
                // 2-part `pkg.item` and 3+ part `lib.pkg.item` selected names:
                // resolve the item inside the named package.
                {
                    let parts: Vec<&str> = name.split('.').collect();
                    if parts.len() >= 2 {
                        let pkg = if parts.len() >= 3 {
                            parts[parts.len() - 2].to_ascii_lowercase()
                        } else {
                            parts[0].to_ascii_lowercase()
                        };
                        let item = parts[parts.len() - 1].to_ascii_lowercase();
                        if let Some(p) = self.pkgs.get(&pkg).cloned() {
                            if let Some((_, v)) = p.consts.iter().find(|(n, _)| *n == item) {
                                return Expr::Lit(v.clone());
                            }
                            if let Some(f) = p
                                .functions
                                .iter()
                                .find(|f| f.name.to_ascii_lowercase() == item)
                            {
                                return Expr::Call {
                                    name: f.name.clone(),
                                    args: vec![Expr::Lit(Value::x(32)); f.args.len()],
                                    shapes: Vec::new(),
                                };
                            }
                            if p.types.iter().any(|(n, _)| *n == item) {
                                return Expr::Lit(Value::x(32));
                            }
                            // Package-level signal selected through the package
                            // (`Manager.Cyc`, `Subordinate.Adr`): materialize a
                            // module signal under the qualified name so two
                            // instances of the same generic package stay distinct.
                            if let Some((_, tn)) = p.sig_types.iter().find(|(n, _)| *n == item) {
                                let tn = tn.clone();
                                let w = p
                                    .sig_widths
                                    .iter()
                                    .find(|(n, _)| *n == item)
                                    .map(|(_, w)| *w)
                                    .unwrap_or(0)
                                    .max(match self.types.get(&tn) {
                                        Some(TypeInfo::Vector { .. }) => {
                                            self.type_widths.get(&tn).copied().unwrap_or(1).max(1)
                                        }
                                        Some(TypeInfo::Record { fields }) => {
                                            fields.iter().map(|f| f.1).sum::<usize>().max(1)
                                        }
                                        _ => 1,
                                    });
                                let qname = format!("{pkg}.{item}");
                                let id = self.ensure_global_sig(&qname);
                                self.sig_width.insert(id, w);
                                if let Some(rn) = self.as_record(&tn) {
                                    self.sig_rec.insert(id, rn);
                                }
                                return Expr::Sig(id);
                            }
                        }
                        // A package used as a value (package actual in a generic
                        // map: `generic map (P => work.my_pkg)`) — inert placeholder.
                        if self.pkgs.contains_key(&item) {
                            return Expr::Lit(Value::x(32));
                        }
                    }
                }
                // Bare physical unit name (`ns`, `ps`, …) as a literal of one
                // unit, in femtoseconds.
                if matches!(
                    key.as_str(),
                    "fs" | "ps"
                        | "ns"
                        | "us"
                        | "ms"
                        | "sec"
                        | "s"
                        | "min"
                        | "hr"
                        | "um"
                        | "hz"
                        | "khz"
                        | "mhz"
                        | "ghz"
                ) {
                    // Physical unit constants (`ns`, `ms`, ...) are values of
                    // type `time` — 64-bit, else `30 ms` truncates to µs.
                    return Expr::Lit(Value::from_u64(64, time_unit_fs(&key)));
                } // Bare user-declared physical unit name (`mm`, `cm`,
                  // `angstrom`, ...): a literal of one unit, in base units.
                if let Some(v) = self.phys_units.get(key.as_str()).copied() {
                    return Expr::Lit(Value::from_u64(64, v));
                }

                // Bare state-variable reference inside a protected-type body:
                // read the field of `$this` (`$getf::t::field@lsb,w`).
                if let Some(t) = self.prot_body_ctx.clone() {
                    if let Some(pt) = self.prot_types.get(&t) {
                        if let Some((_, w, lsb, _)) =
                            pt.fields.iter().find(|(n, _, _, _)| *n == key)
                        {
                            return Expr::Call {
                                name: format!("$getf::{}::{}@{},{}", t, key, lsb, w),
                                args: vec![Expr::Call {
                                    name: "$this".into(),
                                    args: vec![],
                                    shapes: Vec::new(),
                                }],
                                shapes: Vec::new(),
                            };
                        }
                    }
                }
                // Package-level shared variable read from a package-body
                // subprogram: placeholder (not simulated).
                if self.pkg_sig_types.contains_key(&key) {
                    return Expr::Lit(Value::x(1));
                }
                // Inside a protected-type body, unresolved names are common
                // (conversions to the protected object, use'd package items,
                // access-type fields): placeholder, no diagnostic.
                if self.prot_body_ctx.is_some() {
                    return Expr::Lit(Value::x(32));
                }
                self.err(e.span(), format!("unknown identifier '{}'", base));
                Expr::Lit(Value::x(1))
            }
            SyntaxKind::BinExpr => {
                let kids = real_children(e);
                let exprs: Vec<&SyntaxNode> =
                    kids.iter().filter(|k| is_expr_node(k.kind())).collect();
                let op_text: String = kids
                    .iter()
                    .filter(|k| k.kind() == SyntaxKind::Punct || k.kind() == SyntaxKind::Ident)
                    .map(|k| self.kw(k))
                    .collect::<Vec<_>>()
                    .join("");
                if op_text == "&" && exprs.len() == 2 {
                    // Concatenation.
                    return Expr::Concat(vec![
                        self.lower_expr(exprs[0], env),
                        self.lower_expr(exprs[1], env),
                    ]);
                }
                let Some(op) = vhdl_binop(&op_text) else {
                    self.err(e.span(), format!("unsupported operator '{}'", op_text));
                    return Expr::Lit(Value::x(1));
                };
                if exprs.len() == 2 {
                    let le = self.lower_expr(exprs[0], env);
                    let re = self.lower_expr(exprs[1], env);
                    self.mark_signed_compare(op, le, re)
                } else {
                    Expr::Lit(Value::x(1))
                }
            }
            SyntaxKind::UnaryExpr => {
                let kids = real_children(e);
                let op_text = kids
                    .iter()
                    .find(|k| k.kind() == SyntaxKind::Punct || k.kind() == SyntaxKind::Ident)
                    .map(|k| self.kw(k))
                    .unwrap_or_default();
                let operand = kids.iter().find(|k| is_expr_node(k.kind()));
                let Some(op) = vhdl_unop(&op_text) else {
                    self.err(
                        e.span(),
                        format!("unsupported unary operator '{}'", op_text),
                    );
                    return Expr::Lit(Value::x(1));
                };
                match operand {
                    Some(o) => {
                        let oe = self.lower_expr(o, env);
                        // `-literal` is a signed universal integer: fold to a
                        // signed-marked literal so ordered comparisons are
                        // signed (`-1 >= 0` is false, not 4294967295 >= 0).
                        if op == UnOp::Neg {
                            if let Expr::Lit(v) = &oe {
                                if let Some(n) = v.to_u64() {
                                    let w = v.width().max(1);
                                    let mask = if w >= 64 { u64::MAX } else { (1u64 << w) - 1 };
                                    let neg = (!(n & mask)).wrapping_add(1) & mask;
                                    return Expr::Signed(Box::new(Expr::Lit(Value::from_u64(
                                        w, neg,
                                    ))));
                                }
                            }
                        }
                        Expr::Un {
                            op,
                            e: Box::new(oe),
                        }
                    }
                    None => Expr::Lit(Value::x(1)),
                }
            }
            SyntaxKind::Call => self.lower_call(e, env),
            SyntaxKind::PartSelect => {
                // name ( msb dir lsb ) — the base name is not a bound expr.
                // A selected-chain base (`c_arr(0).f(0)(0 TO 15)`) arrives as
                // a Call child instead of a bare Name: the first expression
                // child is then the base and the remaining ones the bounds.
                let kids = real_children(e);
                let name_pos = kids.iter().position(|k| k.kind() == SyntaxKind::Name);
                let (base, bound_nodes): (Option<SyntaxNode>, Vec<SyntaxNode>) = match name_pos {
                    Some(np) => (
                        Some(kids[np].clone()),
                        kids.iter()
                            .enumerate()
                            .filter(|(i, k)| Some(*i) != Some(np) && is_expr_node(k.kind()))
                            .map(|(_, k)| k.clone())
                            .collect(),
                    ),
                    None => {
                        let exprs: Vec<SyntaxNode> = kids
                            .iter()
                            .filter(|k| is_expr_node(k.kind()))
                            .cloned()
                            .collect();
                        match exprs.len() {
                            n if n >= 3 => (Some(exprs[0].clone()), exprs[1..].to_vec()),
                            _ => (None, exprs),
                        }
                    }
                };
                if bound_nodes.len() < 2 {
                    return Expr::Lit(Value::x(1));
                }
                let dir = kids
                    .iter()
                    .find(|k| k.kind() == SyntaxKind::Ident)
                    .map(|k| self.kw(k))
                    .unwrap_or_else(|| "downto".to_string());
                let (msb, lsb) = if dir == "downto" {
                    (bound_nodes[0].clone(), bound_nodes[1].clone())
                } else {
                    (bound_nodes[1].clone(), bound_nodes[0].clone())
                };
                let base_e = match &base {
                    Some(b) => self.lower_expr(b, env),
                    None => Expr::Lit(Value::x(1)),
                };
                // Element slice on a memory-backed array (`int_sig(0 to
                // DELAY-2)`): the kernel reads element k as word k, packed
                // MSB-first (element 0 = most significant bits), so the
                // element bounds must map to that bit range — using them as
                // flat bit indices selects from the wrong end and a shift
                // register packs X into its first word forever. Element e
                // occupies bits [total-1-e·w … total-(e+1)·w]; `sig_width`
                // holds the element width, `mems` the depth.
                if let Expr::Sig(sid) = &base_e {
                    if let (Some(&depth), Some(&ew)) = (self.mems.get(sid), self.sig_width.get(sid))
                    {
                        // `lsb`/`msb` hold the source range's low/high bounds
                        // in both directions (normalized above), and the word
                        // formulas below want (low element, high element):
                        // element e occupies bits [total-1-e·w … total-(e+1)·w],
                        // so the slice spans element `lo`'s top bit down to
                        // element `hi`'s bottom bit regardless of direction.
                        let (e_lo, e_hi) = (lsb.clone(), msb.clone());
                        let total = depth.saturating_mul(ew).max(1);
                        let e_lo = self.lower_expr(&e_lo, env);
                        let e_hi = self.lower_expr(&e_hi, env);
                        let word = |base: u64, e: Expr| Expr::Bin {
                            op: BinOp::Sub,
                            l: Box::new(Expr::Lit(Value::from_u64(32, base))),
                            r: Box::new(Expr::Bin {
                                op: BinOp::Mul,
                                l: Box::new(e),
                                r: Box::new(Expr::Lit(Value::from_u64(32, ew as u64))),
                            }),
                        };
                        return Expr::Sel {
                            e: Box::new(base_e),
                            msb: Box::new(word(total as u64 - 1, e_lo)),
                            lsb: Box::new(word(total as u64 - ew as u64, e_hi)),
                        };
                    }
                }
                // Element addressing on a plain (non-memory) vector: the
                // bounds are element positions, so they must remap for an
                // ascending range (element k = value bit W-1-(k-lo)·ew —
                // element 0 is the most significant, mirroring the
                // memory-backed mapping above) and scale by the element
                // width (`string` characters are 8 bits). A descending
                // 1-bit range from 0 addresses value bits directly and
                // keeps the raw lowering.
                if let Expr::Sig(sid) = &base_e {
                    if !self.mems.contains_key(sid) {
                        // Dynamic first: a shape-bound formal addresses
                        // through its hidden shape slots — the actual's low
                        // bound and direction, bound per call — so the slice
                        // lands at the actual's layout, whatever it is.
                        if let Some(&(lo_id, asc_id)) = self.formal_dyn.get(sid) {
                            let (msb_e, lsb_e) = super::width::dyn_remapped_sel_bounds(
                                self.slot_width_expr(*sid),
                                self.sig_elem.get(sid).copied().unwrap_or(1).max(1) as u64,
                                Expr::Sig(lo_id),
                                Expr::Sig(asc_id),
                                self.lower_expr(&msb, env),
                                self.lower_expr(&lsb, env),
                            );
                            return Expr::Sel {
                                e: Box::new(base_e),
                                msb: Box::new(msb_e),
                                lsb: Box::new(lsb_e),
                            };
                        }
                        let (lo, asc) = self.sig_asc.get(sid).copied().unwrap_or((0, false));
                        let ew = self.sig_elem.get(sid).copied().unwrap_or(1).max(1) as u64;
                        if asc || lo != 0 || ew > 1 {
                            let w = self.slot_width_expr(*sid);
                            let lo_u = lo.max(0) as u64;
                            // `msb`/`lsb` hold the source range's high/low
                            // bounds; for `to` the left (MSB-most) element is
                            // `lsb`, for `downto` it is `msb`.
                            let (msb_e, lsb_e) = super::width::remapped_sel_bounds(
                                w,
                                ew,
                                lo_u,
                                asc,
                                self.lower_expr(&msb, env),
                                self.lower_expr(&lsb, env),
                            );
                            return Expr::Sel {
                                e: Box::new(base_e),
                                msb: Box::new(msb_e),
                                lsb: Box::new(lsb_e),
                            };
                        }
                    }
                }
                // Slice of a constant (`ASC_DATA(0 to 3)`): remap the index
                // bounds to value-bit positions from the recorded decl
                // direction, like the signal path above.
                if let Some(sel) = self.const_slice_sel(base.as_ref(), &base_e, &msb, &lsb, env) {
                    return sel;
                }
                // Slice of a record field that is itself an array
                // (`r.options(0 to n)`, `r.description(1 to len)`): the
                // bounds are element positions inside the field's packed
                // value. Field-array elements address MSB-first (element 0
                // = most significant bits, matching `$const_idx` reads), so
                // the bounds remap like an ascending signal slice over a
                // carrier of the field's bit width.
                if let Expr::Sel { .. } = &base_e {
                    if let Some((flo, few, fw)) = base
                        .as_ref()
                        .and_then(|b| self.field_array_shape(&self.text(b)))
                    {
                        let w_e = Expr::Lit(Value::from_u64(32, fw));
                        let lo_e = Expr::Lit(Value::from_u64(32, flo.max(0) as u64));
                        let off_left = Expr::Bin {
                            op: BinOp::Sub,
                            l: Box::new(self.lower_expr(&lsb, env)),
                            r: Box::new(lo_e.clone()),
                        };
                        let off_right = Expr::Bin {
                            op: BinOp::Sub,
                            l: Box::new(self.lower_expr(&msb, env)),
                            r: Box::new(lo_e),
                        };
                        let scaled = |e: Expr, k: u64| Expr::Bin {
                            op: BinOp::Mul,
                            l: Box::new(e),
                            r: Box::new(Expr::Lit(Value::from_u64(32, k))),
                        };
                        return Expr::Sel {
                            e: Box::new(base_e),
                            msb: Box::new(Expr::Bin {
                                op: BinOp::Sub,
                                l: Box::new(Expr::Bin {
                                    op: BinOp::Sub,
                                    l: Box::new(w_e),
                                    r: Box::new(Expr::Lit(Value::from_u64(32, 1))),
                                }),
                                r: Box::new(scaled(off_left, few)),
                            }),
                            lsb: Box::new(Expr::Bin {
                                op: BinOp::Sub,
                                l: Box::new(Expr::Lit(Value::from_u64(32, fw))),
                                r: Box::new(scaled(
                                    Expr::Bin {
                                        op: BinOp::Add,
                                        l: Box::new(off_right),
                                        r: Box::new(Expr::Lit(Value::from_u64(32, 1))),
                                    },
                                    few,
                                )),
                            }),
                        };
                    }
                }
                // Slice of a constant-element read (`C_PKT(0)(0 TO 15)` —
                // the base lowers to `$const_idx` over the constant's flat
                // value). Element k packs MSB-first (element 0 = most
                // significant bits, the same rule `$const_idx` applies), so
                // an ascending slice's bounds remap to value-bit positions
                // over a carrier of the element width. Descending slices
                // keep the raw bounds (the flat default mapping).
                if let Expr::Call { name, args, .. } = &base_e {
                    if name == "$const_idx" && dir == "to" {
                        if let Some(Expr::Lit(ewv)) = args.get(2) {
                            let ew = ewv.to_u64().unwrap_or(1).max(1);
                            let (msb_e, lsb_e) = super::width::remapped_sel_bounds(
                                Expr::Lit(Value::from_u64(32, ew)),
                                1,
                                0,
                                true,
                                self.lower_expr(&msb, env),
                                self.lower_expr(&lsb, env),
                            );
                            return Expr::Sel {
                                e: Box::new(base_e.clone()),
                                msb: Box::new(msb_e),
                                lsb: Box::new(lsb_e),
                            };
                        }
                    }
                }
                Expr::Sel {
                    e: Box::new(base_e),
                    msb: Box::new(self.lower_expr(&msb, env)),
                    lsb: Box::new(self.lower_expr(&lsb, env)),
                }
            }
            SyntaxKind::Attr => {
                // name'attr — only 'length/'high/'low/'range supported statically.
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
                    "length" | "high" | "low" | "left" | "right" => {
                        // Constants/generics: the object's recorded dims (decl
                        // constraint or initializer shape) fold the attribute
                        // even when the type mark is unconstrained. Checked
                        // before the signal path: generic names also sit in
                        // the signal table carrying only their packed slot
                        // width, which would fold `s'length` of a string
                        // generic to 1.
                        if let Some(ex) = self.obj_attr_expr(&bn, &attr) {
                            return ex;
                        }
                        // Statically known for signals: arrays → element count
                        // (mem depth), vectors → bit width.
                        if let Some(id) = self.sig_id(&bn) {
                            if self.uncon_ports.contains(&id) {
                                // Width known only after port connection.
                                return Expr::SigAttr {
                                    sig: id,
                                    attr: match attr.as_str() {
                                        "length" => "length",
                                        "high" => "high",
                                        "low" => "low",
                                        "left" => "left",
                                        _ => "right",
                                    },
                                };
                            }
                            let dims = self.sig_dims.get(&id);
                            let mem_w = self.mems.get(&id).copied();
                            let sig_w = self.sig_width.get(&id).copied();
                            // Subprogram formals are sized by the call's
                            // actuals; a stale `sig_width`/`sig_dims` entry
                            // left by a previously lowered subprogram under
                            // the same slot id must not bake a constant.
                            if self.fn_formals.contains(&id)
                                || (dims.is_none() && mem_w.is_none() && sig_w.is_none())
                            {
                                // Function formals and other not-yet-sized
                                // signals: the width is only known from the
                                // bound value at runtime (`arg'length` inside
                                // an unconstrained-function body). A baked
                                // constant truncated
                                // `conv_unsigned(count, arg'length)` to a
                                // 1-bit shift count. `length` is an element
                                // count, but the runtime and const-eval both
                                // evaluate `SigAttr` as the bound value's bit
                                // width — divide by the element width
                                // (`string`: 8) right here, where the element
                                // width is known.
                                let sa = Expr::SigAttr {
                                    sig: id,
                                    attr: match attr.as_str() {
                                        "length" => "length",
                                        "high" => "high",
                                        "low" => "low",
                                        "left" => "left",
                                        _ => "right",
                                    },
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
                                return sa;
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
                            let lo: i64 =
                                dims.and_then(|d| d.first().map(|(l, _)| *l)).unwrap_or(0);
                            let v = match attr.as_str() {
                                "length" => n as u64,
                                "high" | "left" => (lo + n as i64 - 1).max(0) as u64,
                                _ => lo.max(0) as u64, // low | right
                            };
                            return Expr::Lit(Value::from_u64(32, v));
                        }
                        // `sig.field[.field...]'length` — record field select on a
                        // record-typed signal/port or record alias: the field's
                        // packed width (first field = MSB).
                        if let Some((b0, rest)) = bn.split_once('.') {
                            let path: Vec<&str> = rest.split('.').collect();
                            let tn = self
                                .sig_id(b0)
                                .and_then(|i| self.sig_rec.get(&i).cloned())
                                .or_else(|| self.alias_rec.get(b0).cloned());
                            if let Some(tn) = tn {
                                if let Some((lsb, w)) = self.record_field_offset(&tn, &path) {
                                    let v = match attr.as_str() {
                                        "length" => w as u64,
                                        "high" | "left" => (lsb + w - 1) as u64,
                                        _ => lsb as u64, // low | right
                                    };
                                    return Expr::Lit(Value::from_u64(32, v));
                                }
                            }
                        }
                        // `alias'length` where the alias targets a field/slice
                        // select with literal bounds.
                        if let Some(Expr::Sel { msb, lsb, .. }) =
                            self.aliases.get(&bn.to_ascii_lowercase())
                        {
                            if let (Some(m), Some(l)) = (self.ce(msb), self.ce(lsb)) {
                                if let (Some(m), Some(l)) = (m.to_u64(), l.to_u64()) {
                                    let v = match attr.as_str() {
                                        "length" => (m as i64 - l as i64).unsigned_abs() + 1,
                                        "high" | "left" => m,
                                        _ => l, // low | right
                                    };
                                    return Expr::Lit(Value::from_u64(32, v));
                                }
                            }
                        }
                        // Constants/generics: the object's recorded dims
                        // (decl constraint or initializer shape) fold the
                        // attribute even when the type mark is unconstrained.
                        if let Some(ex) = self.obj_attr_expr(&bn, &attr) {
                            return ex;
                        }
                        // Type marks and subtypes: `integer'high`, `my_arr'length`, ...
                        self.type_attr(&bn, &attr)
                    }
                    // `s'range` in expression position is only meaningful in a
                    // for-spec (handled there); as a value it is X.
                    "range" | "reverse_range" => Expr::Lit(Value::x(32)),
                    // Report/simulation attributes: placeholder, no warning.
                    "path_name" | "instance_name" | "last_event" | "image" | "reverse_image"
                    | "encoding" | "max_wire_delay" | "subtype" | "simple_name" | "driving"
                    | "driving_value" => Expr::Lit(Value::x(32)),
                    // `sig'quiet` = no event this delta; `sig'active` = event.
                    "quiet" => {
                        if let Some(id) = self.sig_id(&bn) {
                            let s = || Box::new(Expr::Sig(id));
                            return Expr::Un {
                                op: UnOp::Not,
                                e: Box::new(Expr::Bin {
                                    op: BinOp::LogOr,
                                    l: Box::new(Expr::Rising(s())),
                                    r: Box::new(Expr::Falling(s())),
                                }),
                            };
                        }
                        Expr::Lit(Value::x(1))
                    }
                    "active" | "transaction" => {
                        if let Some(id) = self.sig_id(&bn) {
                            let s = || Box::new(Expr::Sig(id));
                            return Expr::Bin {
                                op: BinOp::LogOr,
                                l: Box::new(Expr::Rising(s())),
                                r: Box::new(Expr::Falling(s())),
                            };
                        }
                        Expr::Lit(Value::x(1))
                    }
                    "ascending" => Expr::Lit(Value::from_u64(1, 1)),
                    // `sig'last_value` — value before the current delta.
                    "last_value" => {
                        if let Some(id) = self.sig_id(&bn) {
                            Expr::Prev(Box::new(Expr::Sig(id)))
                        } else {
                            self.warn(
                                e.span(),
                                format!("attribute '{}\\'{}' not supported; X", bn, attr),
                            );
                            Expr::Lit(Value::x(1))
                        }
                    }
                    // `sig'stable` — no event this delta: NOT (rising OR falling).
                    "stable" => {
                        if let Some(id) = self.sig_id(&bn) {
                            let s = || Box::new(Expr::Sig(id));
                            Expr::Un {
                                op: UnOp::Not,
                                e: Box::new(Expr::Bin {
                                    op: BinOp::LogOr,
                                    l: Box::new(Expr::Rising(s())),
                                    r: Box::new(Expr::Falling(s())),
                                }),
                            }
                        } else {
                            self.warn(
                                e.span(),
                                format!("attribute '{}\\'{}' not supported; X", bn, attr),
                            );
                            Expr::Lit(Value::x(1))
                        }
                    }
                    // `sig'event` — true on any value change of sig this delta:
                    // rising OR falling edge (scalar signals).
                    "event" => {
                        if let Some(id) = self.sig_id(&bn) {
                            let s = || Box::new(Expr::Sig(id));
                            Expr::Bin {
                                op: BinOp::LogOr,
                                l: Box::new(Expr::Rising(s())),
                                r: Box::new(Expr::Falling(s())),
                            }
                        } else {
                            self.warn(
                                e.span(),
                                format!("attribute '{}\\'{}' not supported; X", bn, attr),
                            );
                            Expr::Lit(Value::x(1))
                        }
                    }
                    _ => {
                        // User-declared attributes (`attribute a : T;`) have no
                        // tracked value; X without a warning.
                        if self.attr_decls.contains(&attr) {
                            return Expr::Lit(Value::x(32));
                        }
                        self.warn(
                            e.span(),
                            format!("attribute '{}\'{}' not supported; X", bn, attr),
                        );
                        Expr::Lit(Value::x(1))
                    }
                }
            }
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

    /// Single-element addressing `v(k)` on a plain (non-memory) vector with a
    /// recorded index direction or a multi-bit element width: ascending
    /// ranges address mirrored (element k = value bit W-1-(k-lo)·ew) and
    /// `string` characters are 8 bits, so the element must lower to a sized
    /// `Sel`, not a raw 1-bit `Idx`. `None` keeps `Idx` — a 1-bit descending
    /// range from 0, where raw bit addressing is already correct.
    fn elem_sel(&self, sid: SigId, k: &Expr) -> Option<Expr> {
        let ew = self.sig_elem.get(&sid).copied().unwrap_or(1).max(1) as u64;
        // A shape-bound formal addresses through its hidden shape slots —
        // the actual's low bound and direction, bound per call — so element
        // k lands at the actual's layout, whatever it is.
        if let Some(&(lo_id, asc_id)) = self.formal_dyn.get(&sid) {
            let (msb, lsb) = super::width::dyn_remapped_sel_bounds(
                self.slot_width_expr(sid),
                ew,
                Expr::Sig(lo_id),
                Expr::Sig(asc_id),
                k.clone(),
                k.clone(),
            );
            return Some(Expr::Sel {
                e: Box::new(Expr::Sig(sid)),
                msb: Box::new(msb),
                lsb: Box::new(lsb),
            });
        }
        let (lo, asc) = self.sig_asc.get(&sid).copied().unwrap_or((0, false));
        if !asc && lo == 0 && ew == 1 {
            return None;
        }
        let w = self.slot_width_expr(sid);
        let lo = lo.max(0) as u64;
        let off = || Expr::Bin {
            op: BinOp::Sub,
            l: Box::new(k.clone()),
            r: Box::new(Expr::Lit(Value::from_u64(32, lo))),
        };
        let scaled = |e: Expr| Expr::Bin {
            op: BinOp::Mul,
            l: Box::new(e),
            r: Box::new(Expr::Lit(Value::from_u64(32, ew))),
        };
        // element k covers value bits [W-1-(k-lo)·ew … W-(k-lo+1)·ew]
        // ascending, [(k-lo)·ew … (k-lo+1)·ew-1] descending.
        let (msb, lsb) = if asc {
            (
                Expr::Bin {
                    op: BinOp::Sub,
                    l: Box::new(Expr::Bin {
                        op: BinOp::Sub,
                        l: Box::new(w.clone()),
                        r: Box::new(Expr::Lit(Value::from_u64(32, 1))),
                    }),
                    r: Box::new(scaled(off())),
                },
                Expr::Bin {
                    op: BinOp::Sub,
                    l: Box::new(w),
                    r: Box::new(scaled(Expr::Bin {
                        op: BinOp::Add,
                        l: Box::new(off()),
                        r: Box::new(Expr::Lit(Value::from_u64(32, 1))),
                    })),
                },
            )
        } else {
            (
                Expr::Bin {
                    op: BinOp::Add,
                    l: Box::new(scaled(off())),
                    r: Box::new(Expr::Lit(Value::from_u64(32, ew - 1))),
                },
                scaled(off()),
            )
        };
        Some(Expr::Sel {
            e: Box::new(Expr::Sig(sid)),
            msb: Box::new(msb),
            lsb: Box::new(lsb),
        })
    }

    fn lower_call(&mut self, e: &SyntaxNode, env: &HashMap<String, Value>) -> Expr {
        let kids = real_children(e);
        // `integer'image(x)` arrives as Call(Attr(Name integer, 'image), args).
        // The argument may itself be an attribute (`sig'last_value`, …), so
        // only the callee Attr is excluded — and only when it appears before
        // the callee Name (`a(a'high)` has the Attr as an *argument*).
        let name_pos0 = kids.iter().position(|k| k.kind() == SyntaxKind::Name);
        let attr_pos = kids
            .iter()
            .position(|k| k.kind() == SyntaxKind::Attr)
            .filter(|&i| name_pos0.is_none_or(|np| i < np));
        if let Some(ap) = attr_pos {
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
            // Array attributes with an explicit dimension — `A'length(1)`,
            // `A'high(2)` (LRM 1076 §5.2.1) arrive as Call(Attr(prefix, attr),
            // [dim]). Fold when the dimension's bounds are statically known
            // (signals, constants/generics with known dims, array types and
            // subtypes); otherwise X.
            if matches!(
                attr_name.as_str(),
                "length" | "high" | "low" | "left" | "right" | "ascending"
            ) {
                let arg = kids
                    .iter()
                    .enumerate()
                    .find(|(i, k)| *i != ap && is_expr_node(k.kind()))
                    .map(|(_, k)| k);
                if let Some(a) = arg {
                    let dim_e = self.lower_expr(a, env);
                    let dim = self.ce(&dim_e).and_then(|v| v.to_u64()).unwrap_or(1).max(1) as usize;
                    let base = ak
                        .iter()
                        .find(|k| k.kind() == SyntaxKind::Name)
                        .map(|k| self.kw(k))
                        .unwrap_or_default();
                    if let Some(v) = self.attr_dim_value(&base, &attr_name, dim) {
                        if attr_name == "ascending" {
                            return Expr::Lit(Value::from_u64(1, v));
                        }
                        return Expr::Lit(Value::from_u64(32, v));
                    }
                    return Expr::Lit(Value::x(if attr_name == "ascending" { 1 } else { 32 }));
                }
            }
            if attr_name == "image" {
                if let Some(arg) = kids
                    .iter()
                    .enumerate()
                    .find(|(i, k)| *i != ap && is_expr_node(k.kind()))
                    .map(|(_, k)| k)
                {
                    return self.attr_image_expr(arg, env, &prefix);
                }
            }
            // `type'val(x)` / `type'pos(x)` — numeric identity, but 'val takes
            // the prefix type's width (character'val(65) is 8 bits). Only take
            // this path when the argument resolves in the current scope — a
            // bare name that does not (e.g. an entity-declared enum literal
            // used in an architecture) keeps the old call fallback.
            if attr_name == "val" || attr_name == "pos" {
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
                if let (Some(arg), true) = (arg, resolvable) {
                    let inner = Expr::Call {
                        name: format!("${attr_name}"),
                        args: vec![self.lower_expr(arg, env)],
                        shapes: Vec::new(),
                    };
                    if attr_name == "val" {
                        let prefix = ak.iter().find(|k| k.kind() == SyntaxKind::Name).cloned();
                        if let Some(pn) = prefix {
                            let (w, _, _, _) = self.resolve_type(&pn, None, e.span());
                            return Expr::Resize {
                                e: Box::new(inner),
                                width: w.max(1),
                                signed: false,
                            };
                        }
                    }
                    return inner;
                }
            }
        }
        // The callee is the first Name child (absent for attribute calls like
        // `integer'image(x)`, where kids are [Attr, args...]). Arguments are
        // every other expression child — plain names included (`rising_edge(clk)`).
        let name_pos = kids.iter().position(|k| k.kind() == SyntaxKind::Name);
        let name = name_pos.map(|i| self.text(&kids[i])).unwrap_or_default();
        let fname = name
            .rsplit('.')
            .next()
            .unwrap_or(name.as_str())
            .to_ascii_lowercase();
        let arg_nodes: Vec<SyntaxNode> = kids
            .iter()
            .enumerate()
            .filter(|(i, k)| Some(*i) != name_pos && Some(*i) != attr_pos && is_expr_node(k.kind()))
            .map(|(_, k)| k.clone())
            .collect();
        let args: Vec<Expr> = arg_nodes.iter().map(|k| self.lower_expr(k, env)).collect();
        // `c_vec(0).f(0)` — a selected-name chain as the callee: the parser
        // folds the prefix into one dotted Name, so only the call path sees
        // it. Route const-rooted chains through the chain walker (it rejects
        // function roots), which interleaves field and index segments over
        // the constant's flat value — the same expression the runtime and
        // const-eval already fold. Signal/alias roots keep their existing
        // per-shape handling below.
        if name.contains('.') {
            let root = name
                .split(['.', '('])
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            if !root.is_empty()
                && self.consts.contains_key(&root)
                && !self.funcs.contains_key(&root)
            {
                if let Some(ex) = self.lower_selected_chain(e, env) {
                    return ex;
                }
            }
        }
        // `.all(i)` dereference of an access-to-array: `L.all(i)` -> L(i).
        if fname == "all"
            && name.len() > 4
            && name.as_bytes()[name.len() - 4] == b'.'
            && !args.is_empty()
        {
            let prefix = &name[..name.len() - 4];
            if let Some(id) = self.sig_id(prefix) {
                return Expr::Idx {
                    e: Box::new(Expr::Sig(id)),
                    i: Box::new(args[0].clone()),
                };
            }
        }
        // A record/array-typed constant element read routes through
        // `lower_const_array_index` below (`$const_idx` over the literal —
        // the const-eval cannot see a materialized hidden signal).
        let base_id = self
            .sig_id(&name)
            .or_else(|| match self.aliases.get(&fname) {
                Some(Expr::Sig(id)) => Some(*id),
                _ => None,
            });
        // Dotted callee `rec.field(args)` or `pkg_var.method(args)`: if the
        // base names a signal/record or a package-level signal, this is a
        // method call — lower as a plain call of the field name.
        if base_id.is_none() && name.contains('.') {
            let base = name.split('.').next().unwrap_or(name.as_str());
            // `rec.field(i)` where `field` is an array-typed field of a
            // record signal: index the field's slice. Emitting the method
            // call fallback here would call a function named after the
            // field (→ X at runtime) and drop the select entirely.
            if args.len() == 1 {
                if let Some(id) = self.sig_id(base) {
                    if let Some(tn) = self.sig_rec.get(&id).cloned() {
                        if let Some(TypeInfo::Record { fields }) = self.types.get(&tn) {
                            let fl = fname.to_ascii_lowercase();
                            let total: usize = fields.iter().map(|f| f.1).sum();
                            let mut run = 0usize;
                            for (fn_, w, _) in fields {
                                let lsb = total - run - w;
                                if fn_.to_ascii_lowercase() == fl {
                                    let sel = Expr::Sel {
                                        e: Box::new(Expr::Sig(id)),
                                        msb: Box::new(Expr::Lit(Value::from_u64(
                                            32,
                                            (lsb + w - 1) as u64,
                                        ))),
                                        lsb: Box::new(Expr::Lit(Value::from_u64(32, lsb as u64))),
                                    };
                                    return Expr::Idx {
                                        e: Box::new(sel),
                                        i: Box::new(args[0].clone()),
                                    };
                                }
                                run += w;
                            }
                        }
                    }
                }
            }
            if self.sig_id(base).is_some()
                || self.pkg_sig_types.contains_key(&base.to_ascii_lowercase())
            {
                return Expr::Call {
                    name: fname.clone(),
                    args,
                    shapes: Vec::new(),
                };
            }
        }
        // Fully-qualified call `work.pkg.f(...)` with no `use` clause for pkg:
        // pull the package's subprogram into scope so the call resolves (and
        // can be const-evaluated in a constant initializer).
        if !self.funcs.contains_key(&fname) && name.contains('.') {
            let parts: Vec<&str> = name.split('.').collect();
            if parts.len() >= 3 {
                let unit = parts[parts.len() - 2].to_ascii_lowercase();
                if let Some(p) = self.pkgs.get(&unit).cloned() {
                    self.merge_package(&p, Some(&fname), false);
                }
            }
        }
        if base_id.is_none() && !args.is_empty() {
            if let Some(e) = self.lower_const_array_index(&fname, &args) {
                return e;
            }
        }
        // `obj.m(args)` on a protected-type object → `$method::t::m(obj, …)`.
        if let Some(dot) = name.find('.') {
            let (base, m) = (&name[..dot], name[dot + 1..].to_ascii_lowercase());
            if let Some(id) = self.sig_id(base) {
                if let Some(t) = self.sig_prot.get(&id).cloned() {
                    let is_method = self
                        .prot_types
                        .get(&t)
                        .is_some_and(|p| p.methods.contains(&m));
                    if is_method {
                        let mut args2 = vec![Expr::Sig(id)];
                        args2.extend(args.iter().cloned());
                        return Expr::Call {
                            name: format!("$method::{}::{}", t, m),
                            args: args2,
                            shapes: Vec::new(),
                        };
                    }
                    // Unknown member on a protected object — or any member of
                    // a protected type from a use'd package: placeholder.
                    return Expr::Lit(Value::x(32));
                }
            } else if let Some(tn) = self.pkg_sig_types.get(base) {
                // Package-level shared variable of protected type: placeholder.
                if matches!(self.types.get(tn), Some(TypeInfo::Handle)) {
                    return Expr::Lit(Value::x(32));
                }
            } else if let Some(t) = self.prot_body_ctx.clone() {
                // `st.field(args)` rooted at a protected-type state variable:
                // placeholder (not simulated).
                if self.prot_types.get(&t).is_some_and(|pt| {
                    pt.fields
                        .iter()
                        .any(|(n, _, _, _)| n.eq_ignore_ascii_case(base))
                }) {
                    return Expr::Lit(Value::x(32));
                }
            }
        }
        // Bare `m(args)` inside a protected-type body → self-call on `$this`.
        if let Some(t) = self.prot_body_ctx.clone() {
            if self
                .prot_types
                .get(&t)
                .is_some_and(|p| p.methods.contains(&fname))
            {
                let mut args2 = vec![Expr::Call {
                    name: "$this".into(),
                    args: vec![],
                    shapes: Vec::new(),
                }];
                args2.extend(args.iter().cloned());
                return Expr::Call {
                    name: format!("$method::{}::{}", t, fname),
                    args: args2,
                    shapes: Vec::new(),
                };
            }
        }
        match fname.as_str() {
            // Indexed write/read of a signal: v(i) → Idx; multi-dimensional
            // v(i, j, …) → flat index ((i-lo0)*n1 + (j-lo1))*… via sig_dims.
            // A name that is also a declared function stays a call in call
            // position: inside the function's own body the name resolves to
            // the result slot (slot 0), which must not turn a recursive call
            // into an index of that slot.
            _ if base_id.is_some() && !args.is_empty() && !self.funcs.contains_key(&fname) => {
                let base = base_id.expect("arm guard checked base_id.is_some()");
                if let Some(dims) = self.sig_dims.get(&base).cloned() {
                    if dims.len() == args.len() {
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
                        return Expr::Idx {
                            e: Box::new(Expr::Sig(base)),
                            i: Box::new(flat.expect("dims.len() == args.len() and args is non-empty, so the fold assigned flat")),
                        };
                    }
                }
                if args.len() == 1 {
                    let k = args
                        .into_iter()
                        .next()
                        .expect("match guard requires exactly one argument");
                    if let Some(sid) = base_id {
                        if !self.mems.contains_key(&sid) {
                            if let Some(se) = self.elem_sel(sid, &k) {
                                return se;
                            }
                        }
                    }
                    return Expr::Idx {
                        e: Box::new(Expr::Sig(
                            base_id.expect("arm guard checked base_id.is_some()"),
                        )),
                        i: Box::new(k),
                    };
                }
                Expr::Call {
                    name: fname,
                    args,
                    shapes: Vec::new(),
                }
            }
            // Shift/rotate function forms from std_logic_arith & co.
            "shift_left" | "sll" | "sla" if args.len() == 2 => Expr::Bin {
                op: BinOp::Shl,
                l: Box::new(args[0].clone()),
                r: Box::new(args[1].clone()),
            },
            "shift_right" | "srl" if args.len() == 2 => Expr::Bin {
                op: BinOp::Shr,
                l: Box::new(args[0].clone()),
                r: Box::new(args[1].clone()),
            },
            "sra" if args.len() == 2 => Expr::Bin {
                op: BinOp::Sar,
                l: Box::new(args[0].clone()),
                r: Box::new(args[1].clone()),
            },
            "rotate_left" | "rol" if args.len() == 2 => Expr::Bin {
                op: BinOp::Rol,
                l: Box::new(args[0].clone()),
                r: Box::new(args[1].clone()),
            },
            "rotate_right" | "ror" if args.len() == 2 => Expr::Bin {
                op: BinOp::Ror,
                l: Box::new(args[0].clone()),
                r: Box::new(args[1].clone()),
            },
            // Vendor-package vector shifts: `SHR(vec, n)` / `SHL(vec, n)`
            // from the std_logic_arith idiom play the same role as
            // shift_right/shift_left — fold them to plain shifts so they
            // never reach runtime dispatch. Delegating overload bodies
            // (`RETURN SHR(arg, conv_unsigned(count, arg'length))`) have no
            // type-correct dispatch target — the visible overloads are the
            // delegators themselves — and a folded shift terminates by
            // construction. A record-typed first actual
            // (shr(float_t, natural) families) must stay a real call: the
            // record's flattened width can coincide with a vector's, and a
            // folded shift would mangle the record's fields.
            "shr" | "shl" if args.len() == 2 => {
                let record = match &args[0] {
                    Expr::Sig(s) => self.sig_rec.contains_key(s),
                    _ => false,
                };
                if record {
                    Expr::Call {
                        name: fname,
                        args,
                        shapes: Vec::new(),
                    }
                } else {
                    Expr::Bin {
                        op: if fname == "shl" {
                            BinOp::Shl
                        } else {
                            BinOp::Shr
                        },
                        l: Box::new(args[0].clone()),
                        r: Box::new(args[1].clone()),
                    }
                }
            }
            "rising_edge" if args.len() == 1 => Expr::Rising(Box::new(
                args.into_iter()
                    .next()
                    .expect("match guard requires exactly one argument"),
            )),
            "falling_edge" if args.len() == 1 => Expr::Falling(Box::new(
                args.into_iter()
                    .next()
                    .expect("match guard requires exactly one argument"),
            )),
            "to_integer" | "conv_integer" | "integer" | "natural" | "positive"
                if args.len() == 1 =>
            {
                Expr::ToInt(Box::new(
                    args.into_iter()
                        .next()
                        .expect("match guard requires exactly one argument"),
                ))
            }
            // Real/fixed/float conversions: `x * 1.0` (real-typed f64).
            // `real(x)` is a value conversion (LRM 8.6) — integer bits do
            // NOT pass through unchanged; a bit-identical passthrough made
            // `RETURN conv_float(real(arg))` call itself with the same
            // integer and recurse forever.
            "to_real" | "real" | "to_ufixed" | "to_sfixed" | "to_float" if !args.is_empty() => {
                as_real(
                    args.into_iter()
                        .next()
                        .expect("match guard requires a non-empty argument list"),
                )
            }
            "resize" if args.len() == 3 => as_real(
                args.into_iter()
                    .next()
                    .expect("match guard requires exactly three arguments"),
            ),
            // `to_slv(x)`: bit-identical view as std_logic_vector.
            "to_slv" if args.len() == 1 => args
                .into_iter()
                .next()
                .expect("match guard requires exactly one argument"),
            "to_unsigned" | "conv_unsigned" | "conv_std_logic_vector" if args.len() == 2 => {
                let w = self
                    .ce_env(&args[1], env)
                    .and_then(|w| w.to_u64())
                    .unwrap_or(32) as usize;
                Expr::Resize {
                    e: Box::new(
                        args.into_iter()
                            .next()
                            .expect("match guard requires exactly two arguments"),
                    ),
                    width: w,
                    signed: false,
                }
            }
            "to_signed" | "conv_signed" if args.len() == 2 => {
                let w = self
                    .ce_env(&args[1], env)
                    .and_then(|w| w.to_u64())
                    .unwrap_or(32) as usize;
                Expr::Resize {
                    e: Box::new(
                        args.into_iter()
                            .next()
                            .expect("match guard requires exactly two arguments"),
                    ),
                    width: w,
                    signed: true,
                }
            }
            "resize" if args.len() == 2 => {
                let w = self
                    .ce_env(&args[1], env)
                    .and_then(|w| w.to_u64())
                    .unwrap_or(32) as usize;
                let inner = &args[0];
                let signed = match inner {
                    Expr::Sig(s) => self.sig_signed.get(s).copied().unwrap_or(false),
                    Expr::Signed(_) => true,
                    Expr::Resize { signed, .. } => *signed,
                    _ => false,
                };
                Expr::Resize {
                    e: Box::new(
                        args.into_iter()
                            .next()
                            .expect("match guard requires exactly two arguments"),
                    ),
                    width: w,
                    signed,
                }
            }
            // `signed(x)` keeps the bits but marks signed semantics.
            "signed" if args.len() == 1 => Expr::Signed(Box::new(
                args.into_iter()
                    .next()
                    .expect("match guard requires exactly one argument"),
            )),
            // Other type conversions are bit-identical.
            "std_logic_vector" | "std_ulogic_vector" | "unsigned" | "bit_vector" | "std_logic"
            | "bit" | "boolean" | "string" | "char"
                if args.len() == 1 =>
            {
                args.into_iter()
                    .next()
                    .expect("match guard requires exactly one argument")
            }
            // Alias to a subprogram: `alias a is f[...];` — calls to `a(...)`
            // become calls to the target `f(...)`.
            // Operator-symbol call: `"<"(a, b)`, `"+"(x, y)`.
            _ if args.len() == 2 && vhdl_binop(fname.trim_matches('"')).is_some() => {
                let op = vhdl_binop(fname.trim_matches('"'))
                    .expect("arm guard checked vhdl_binop(...).is_some()");
                self.mark_signed_compare(op, args[0].clone(), args[1].clone())
            }
            _ if self
                .aliases
                .get(&fname)
                .is_some_and(|a| matches!(a, Expr::Call { .. })) =>
            {
                match self
                    .aliases
                    .get(&fname)
                    .expect("arm guard checked that this alias is a Call")
                {
                    Expr::Call { name: target, .. } => {
                        let shapes = self.call_shape_binds(target, &args, &arg_nodes);
                        Expr::Call {
                            name: target.clone(),
                            args,
                            shapes,
                        }
                    }
                    _ => unreachable!(),
                }
            }
            _ => {
                let shapes = self.call_shape_binds(&fname, &args, &arg_nodes);
                Expr::Call {
                    name: fname,
                    args,
                    shapes,
                }
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

    fn lower_aggregate(
        &mut self,
        e: &SyntaxNode,
        env: &HashMap<String, Value>,
        width: usize,
    ) -> Expr {
        // `(others => x)` → replicate x; `(a, b, c)` → concat.
        let kids = real_children(e);
        if kids.iter().any(|k| k.kind() == SyntaxKind::Others) {
            let val = kids.iter().rev().find(|k| is_expr_node(k.kind()));
            return match val {
                Some(v) => {
                    let inner = self.lower_expr(v, env);
                    // Replicate by elements: a character value (`' '`) is
                    // 8 bits, a std_logic value 1 — the target `width` is
                    // total bits, so the count is width / element width.
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
            };
        }
        // Positional values only: a choice followed by `=>` is an association
        // name, not a value (record-typed targets use lower_record_aggregate).
        let mut parts: Vec<Expr> = Vec::new();
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
            if is_expr_node(k.kind()) && !arrow {
                parts.push(self.lower_expr(k, env));
            }
            i += 1;
        }
        Expr::Concat(parts)
    }
    /// Lower an aggregate against a record type `tn`, recursing into
    /// aggregate-typed fields with their own types (nested record/array
    /// aggregates keep their layout instead of degrading to X). Falls back
    /// to plain `lower_expr` per field when a field is not itself an
    /// aggregate of an aggregate type — so behavior matches
    /// [`Self::lower_record_aggregate`] for scalar fields.
    pub(super) fn lower_typed_record_aggregate(
        &mut self,
        e: &SyntaxNode,
        tn: &str,
        fields: &[(String, usize, usize)],
        env: &HashMap<String, Value>,
    ) -> Expr {
        let kids = real_children(e);
        self.lower_record_agg_kids(&kids, tn, fields, env)
    }

    /// Record-aggregate fold over an explicit element-child list — shared by
    /// [`Self::lower_typed_record_aggregate`] (an `Aggregate` node whose
    /// children are the elements) and the qualified-record path (the parser
    /// folds `T'(field => v, …)` directly into the `Qualified` node, so the
    /// element children must be pre-extracted).
    fn lower_record_agg_kids(
        &mut self,
        kids: &[SyntaxNode],
        tn: &str,
        fields: &[(String, usize, usize)],
        env: &HashMap<String, Value>,
    ) -> Expr {
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
                self.lower_typed_record_aggregate(vnode, tn, &fields, env)
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

    pub(super) fn lower_array_aggregate_specs(
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
        let mut named: Vec<(i64, usize)> = Vec::new();
        let mut positional: Vec<usize> = Vec::new();
        let mut others: Option<usize> = None;
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
            } else if is_expr_node(k.kind()) {
                positional.push(i);
                i += 1;
            } else {
                i += 1;
            }
        }
        let mut slots: Vec<Option<usize>> = vec![None; n];
        for (iv, vi) in named {
            let p = iv - lo;
            if p < 0 || p as usize >= n {
                return None; // choice outside the constraint: legacy path
            }
            slots[p as usize] = Some(vi);
        }
        for (j, &pi) in positional.iter().enumerate() {
            if j >= n {
                return None;
            }
            slots[j] = Some(pi);
        }
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

    pub(super) fn lower_literal(&mut self, e: &SyntaxNode, env: &HashMap<String, Value>) -> Expr {
        let t = self.text(e);
        // Physical literal: `100 ps` — the parser merges number + unit into
        // one Literal; the value is femtoseconds.
        if let Some((num, unit)) = t.split_once(char::is_whitespace) {
            if matches!(
                unit.trim().to_ascii_lowercase().as_str(),
                "fs" | "ps"
                    | "ns"
                    | "us"
                    | "ms"
                    | "sec"
                    | "s"
                    | "min"
                    | "hr"
                    | "um"
                    | "hz"
                    | "khz"
                    | "mhz"
                    | "ghz"
            ) {
                // Integer physical (`100 ps`) or real physical (`2.5 ns`,
                // VHDL-2008) \u2014 the value is femtoseconds.
                let fs = match self.parse_int_literal(num.trim()).and_then(|v| v.to_u64()) {
                    // saturating: huge literals (`1e18 hr`-scale constants) must
                    // not overflow in debug builds
                    Some(n) => n.saturating_mul(time_unit_fs(unit.trim())),
                    None => num
                        .trim()
                        .parse::<f64>()
                        .map(|f| (f * time_unit_fs(unit.trim()) as f64) as u64)
                        .unwrap_or(0),
                };
                // Time is femtoseconds — 64-bit, else `30 ms` overflows 32 bits.
                return Expr::Lit(Value::from_u64(64, fs));
            }
            // User-declared physical unit (`1.5 cm` of a `units ... end units`
            // type): value in the type's base unit, 64-bit.
            if let Some(v) = self.phys_lit_value(num, unit.trim()) {
                if self
                    .phys_units
                    .contains_key(&unit.trim().to_ascii_lowercase())
                {
                    return Expr::Lit(Value::from_u64(64, v));
                }
            }
        }
        // Bit string literal: x"FF" / b"01" / o"7", or sized `32X"0"` / signed
        // `32SB"1"` / unsigned `32UB"1"` (VHDL-2008/2019: strip the leading
        // decimal size and optional `S`/`U` prefix).
        let t: &str = {
            let digits = t.len() - t.trim_start_matches(|c: char| c.is_ascii_digit()).len();
            let mut n = digits;
            if digits > 0 && t.len() > n && matches!(t.as_bytes()[n], b's' | b'S' | b'u' | b'U') {
                n += 1;
            }
            if n > 0
                && t.len() > n
                && matches!(
                    t.as_bytes()[n],
                    b'x' | b'X' | b'b' | b'B' | b'o' | b'O' | b'd' | b'D'
                )
            {
                &t[n..]
            } else {
                t.as_str()
            }
        };
        if t.len() >= 2
            && matches!(
                t.as_bytes()[0],
                b'x' | b'X' | b'b' | b'B' | b'o' | b'O' | b'd' | b'D'
            )
            && t.as_bytes()[1] == b'"'
        {
            let base = match t.as_bytes()[0].to_ascii_lowercase() {
                b'x' => 16,
                b'o' => 8,
                b'd' => 10,
                _ => 2,
            };
            let digits = t[2..t.len() - 1].replace('_', "");
            let digit_bits = match base {
                16 => 4,
                10 => 4, // decimal digit in a D bit string: 4 bits (BCD-style)
                8 => 3,
                _ => 1,
            };
            let width = digits.len() * digit_bits;
            let mut bits = vec![VX; width.max(1)];
            for (i, c) in digits.chars().enumerate() {
                let from_lsb = (digits.len() - 1 - i) * digit_bits;
                match c.to_ascii_lowercase() {
                    'x' | '?' => {}
                    'z' => {
                        for j in 0..digit_bits {
                            let idx = from_lsb + j;
                            if idx < bits.len() {
                                bits[idx] = VZ;
                            }
                        }
                    }
                    _ => {
                        if let Some(d) = c.to_digit(base) {
                            for j in 0..digit_bits {
                                let idx = from_lsb + j;
                                if idx < bits.len() {
                                    bits[idx] = if (d >> j) & 1 == 1 { V1 } else { V0 };
                                }
                            }
                        }
                    }
                }
            }
            return self.lit_vec_mark(Value { bits, real: false });
        }
        if t.starts_with('\'') && t.ends_with('\'') && t.len() == 3 {
            // Character literal. The std_logic alphabet keeps its 1-bit value
            // ('0'/'1'/'z', the rest X) — these literals overwhelmingly drive
            // std_logic signals. Any other character is a `character` value:
            // 8-bit ASCII, so string aggregates pad with real spaces
            // (`(others => ' ')` used to poison the value with X and lose
            // `'length`).
            let c = t.as_bytes()[1];
            let b = match c.to_ascii_lowercase() {
                b'0' => V0,
                b'1' => V1,
                b'z' => VZ,
                b'u' => VU,
                b'-' | b'l' | b'h' | b'w' | b'x' => VX,
                _ => {
                    return Expr::Lit(Value {
                        bits: (0..8)
                            .map(|j| if (c >> j) & 1 == 1 { V1 } else { V0 })
                            .collect(),
                        real: false,
                    });
                }
            };
            return Expr::Lit(Value {
                bits: vec![b],
                real: false,
            });
        }
        if t.starts_with('"') {
            let inner = t.trim_matches('"').replace("\"\"", "\"");
            // In VHDL a string of bit characters is a std_logic_vector value
            // (`"0000"`), not ASCII text — unless the literal sits in a
            // string-typed object's initializer (`constant m : string :=
            // "0x0000"`), where the characters are text: an all-alphabet
            // literal lowered 1 bit/char X-poisons the value and kills
            // `'length` downstream.
            if !self.str_ctx
                && !inner.is_empty()
                && inner.bytes().all(|c| {
                    matches!(
                        c,
                        b'0' | b'1' | b'x' | b'X' | b'z' | b'Z' | b'u' | b'U' | b'w' | b'W'
                            | b'l' | b'L' | b'h' | b'H' | b'-' | b'?'
                    )
                })
                // A lone letter ("h") is far more often the one-character
                // STRING "h" than a 1-bit weak value — u/w/l/h spell words in
                // report text and filenames. Only a multi-character literal
                // ("UWH-") reads as std_ulogic metavalues; a single one is
                // written 'h' (char literal, handled above).
                && !(inner.len() == 1
                    && inner
                        .bytes()
                        .next()
                        .is_some_and(|c| matches!(c.to_ascii_lowercase(), b'u' | b'w' | b'l' | b'h')))
            {
                let mut bits = vec![VX; inner.len()];
                for (i, c) in inner.bytes().enumerate() {
                    bits[inner.len() - 1 - i] = match c.to_ascii_lowercase() {
                        b'0' => V0,
                        b'1' => V1,
                        b'z' => VZ,
                        b'u' => VU,
                        b'w' => VW,
                        b'l' => VL,
                        b'h' => VH,
                        b'-' | b'?' => VDASH, // don't-care, matched by `case?`
                        _ => VX,
                    };
                }
                return self.lit_vec_mark(Value { bits, real: false });
            }
            // String literal: first char in the high byte (MSB-first), so
            // `&`-concatenation and string_of read left to right. An empty
            // literal is zero-width — `'length` of the formal it feeds must
            // be 0, or a guarded `result(1 to arg'length) := arg` runs with
            // a phantom character and clobbers the first result char.
            let bytes = inner.as_bytes();
            let n = bytes.len();
            let mut bits = vec![V0; n * 8];
            for (bi, &b) in bytes.iter().enumerate() {
                for j in 0..8 {
                    bits[(n - 1 - bi) * 8 + j] = if (b >> j) & 1 == 1 { V1 } else { V0 };
                }
            }
            return self.lit_vec_mark(Value { bits, real: false });
        }
        if let Some(v) = self.parse_int_literal(t) {
            return Expr::Lit(v);
        }
        // VHDL digit separators (`2.71828_18284_...`) are not valid in Rust's
        // f64 parse — strip them first.
        let t_clean: String = t.chars().filter(|c| *c != '_').collect();
        if let Ok(v) = t_clean.parse::<f64>() {
            // VHDL real literal (contains '.' or exponent, no base '#').
            if t_clean.contains('.') || t_clean.contains('e') || t_clean.contains('E') {
                return Expr::Real(v);
            }
            self.warn(e.span(), "real literals unsupported; X");
            return Expr::Lit(Value::x(32));
        }
        let _ = env;
        // `null` — the null access value (access types are not simulated).
        if t.eq_ignore_ascii_case("null") {
            return Expr::Lit(Value::x(64));
        }
        self.err(e.span(), format!("unsupported literal '{}'", t));
        Expr::Lit(Value::x(1))
    }
}
