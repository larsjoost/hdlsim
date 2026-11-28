//! VHDL expression lowering: expressions, calls, attribute access,
//! literals, and aggregates. Extracted verbatim from the lowerer body
//! (pure move, no logic changes); decomposition into per-kind helpers
//! follows in separate changes.

use super::*;

mod aggregate;
mod attr_expr;
mod literal;
mod operators;
mod part_sel;

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
        if let Some(ex) = self.lower_attr_callee(e, env, &kids, attr_pos) {
            return ex;
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
