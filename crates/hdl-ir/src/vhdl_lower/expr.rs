//! VHDL expression lowering: expressions, calls, attribute access,
//! literals, and aggregates. Extracted verbatim from the lowerer body
//! (pure move, no logic changes); decomposition into per-kind helpers
//! follows in separate changes.

use super::*;

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
            // Qualified expression `type_mark'(expr)` — a type conversion:
            // lower the operand (the target type only matters for width, which
            // the operand already carries).
            SyntaxKind::Qualified => {
                let kids = real_children(e);
                let rest = &kids[1..];
                // A qualified *aggregate* (`T'(field => v, others => x)`) is a
                // construction, not a conversion — placeholder.
                let is_aggregate = rest.iter().any(|k| k.kind() == SyntaxKind::Others)
                    || kids
                        .iter()
                        .any(|k| k.kind() == SyntaxKind::Punct && self.text(k) == "=")
                    || kids.iter().any(|k| k.kind() == SyntaxKind::Range);
                if is_aggregate {
                    return Expr::Lit(Value::x(32));
                }
                match rest.iter().find(|k| is_expr_node(k.kind())) {
                    Some(k) => self.lower_expr(k, env),
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
                                    };
                                }
                                if let Some((_, w, lsb, _)) =
                                    pt.fields.iter().find(|(n, _, _, _)| *n == last)
                                {
                                    return Expr::Call {
                                        name: format!("$getf::{}::{}@{},{}", t, last, lsb, w),
                                        args: vec![Expr::Sig(id)],
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
                        return Expr::Lit(v.clone());
                    }
                }
                if let Some(id) = self.sig_id(base) {
                    return Expr::Sig(id);
                }
                if let Some(v) = self.consts.get(&key) {
                    return Expr::Lit(v.clone());
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
                                }],
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
                    Expr::Bin {
                        op,
                        l: Box::new(self.lower_expr(exprs[0], env)),
                        r: Box::new(self.lower_expr(exprs[1], env)),
                    }
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
                    Some(o) => Expr::Un {
                        op,
                        e: Box::new(self.lower_expr(o, env)),
                    },
                    None => Expr::Lit(Value::x(1)),
                }
            }
            SyntaxKind::Call => self.lower_call(e, env),
            SyntaxKind::PartSelect => {
                // name ( msb dir lsb ) — the base name is not a bound expr.
                let kids = real_children(e);
                let base_pos = kids.iter().position(|k| k.kind() == SyntaxKind::Name);
                let base = base_pos.map(|i| &kids[i]);
                let exprs: Vec<&SyntaxNode> = kids
                    .iter()
                    .enumerate()
                    .filter(|(i, k)| Some(*i) != base_pos && is_expr_node(k.kind()))
                    .map(|(_, k)| k)
                    .collect();
                if exprs.len() < 2 {
                    return Expr::Lit(Value::x(1));
                }
                let dir = kids
                    .iter()
                    .find(|k| k.kind() == SyntaxKind::Ident)
                    .map(|k| self.kw(k))
                    .unwrap_or_else(|| "downto".to_string());
                let (msb, lsb) = if dir == "downto" {
                    (exprs[0], exprs[1])
                } else {
                    (exprs[1], exprs[0])
                };
                let base_e = match base {
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
                        let (e_lo, e_hi) = if dir == "downto" {
                            (lsb, msb)
                        } else {
                            (msb, lsb)
                        };
                        let total = depth.saturating_mul(ew).max(1);
                        let e_lo = self.lower_expr(e_lo, env);
                        let e_hi = self.lower_expr(e_hi, env);
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
                // Ascending vector (`0 to N-1`): element k addresses value
                // bit W-1-(k-lo) — element 0 is the most significant bit,
                // mirroring the memory-backed mapping above — so a slice
                // (a to b) covers value bits [W-1-(b-lo) … W-1-(a-lo)].
                // Descending ranges already address value bits directly.
                if let Expr::Sig(sid) = &base_e {
                    if !self.mems.contains_key(sid) {
                        if let Some(&(lo, true)) = self.sig_asc.get(sid) {
                            let w = self.sig_width.get(sid).copied().unwrap_or(0).max(1) as u64;
                            let lo = lo.max(0) as u64;
                            let a = self.lower_expr(msb, env);
                            let b = self.lower_expr(lsb, env);
                            // value bit = (w-1) - (element - lo)
                            let off = |e: Expr| Expr::Bin {
                                op: BinOp::Sub,
                                l: Box::new(e),
                                r: Box::new(Expr::Lit(Value::from_u64(32, lo))),
                            };
                            let bit = |e: Expr| Expr::Bin {
                                op: BinOp::Sub,
                                l: Box::new(Expr::Lit(Value::from_u64(32, w - 1))),
                                r: Box::new(e),
                            };
                            return Expr::Sel {
                                e: Box::new(base_e),
                                msb: Box::new(bit(off(b))),
                                lsb: Box::new(bit(off(a))),
                            };
                        }
                    }
                }
                Expr::Sel {
                    e: Box::new(base_e),
                    msb: Box::new(self.lower_expr(msb, env)),
                    lsb: Box::new(self.lower_expr(lsb, env)),
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
                            if dims.is_none() && mem_w.is_none() && sig_w.is_none() {
                                // Function formals and other not-yet-sized
                                // signals: the width is only known from the
                                // bound value at runtime (`arg'length` inside
                                // an unconstrained-function body). A baked
                                // constant truncated
                                // `conv_unsigned(count, arg'length)` to a
                                // 1-bit shift count.
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
            SyntaxKind::Aggregate => self.lower_aggregate(e, env, 32),
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
}
