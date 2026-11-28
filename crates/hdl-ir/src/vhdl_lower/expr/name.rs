//! VHDL name resolution in expression position: the sequential
//! interpretation cascade for `Name` nodes — `.all` stripping, protected-
//! type member selects, structural selected chains, `arr(i).field` indexed
//! field selects, record-field selects over signal/alias/package/constant
//! roots, and plain identifier resolution (scope → signals → constants →
//! aliases → functions → special names → package items → physical units →
//! protected state). Split out of `expr.rs` to keep every function at its
//! standards baseline.

use super::*;

impl<'a> Lower<'a> {
    pub(super) fn lower_name(&mut self, e: &SyntaxNode, env: &HashMap<String, Value>) -> Expr {
        let name = strip_all_derefs(self.text(e));
        // `obj.m` / `obj.field` on a protected-type object in
        // expression position (parameterless method call / field).
        // Unknown members (and package-level shared variables, which
        // have no per-module object yet) stay placeholders.
        if let Some(ex) = self.prot_member_select(&name) {
            return ex;
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
        if let Some(ex) = self.indexed_field_select(e, env) {
            return ex;
        }
        // Record field select: `r.field` (or nested `r.a.b`) on a
        // record-typed signal → slice of the flat record vector
        // (first field = MSB).
        if name.contains('.') {
            if let Some(ex) = self.record_field_select(&name, env) {
                return ex;
            }
        }
        self.resolve_plain_name(e, &name, env)
    }

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

    /// `obj.m` / `obj.field` member select on a protected-type object.
    /// `None` falls through to the remaining interpretations.
    fn prot_member_select(&self, name: &str) -> Option<Expr> {
        let dot = name.rfind('.')?;
        let (base, last) = (&name[..dot], name[dot + 1..].to_ascii_lowercase());
        let base_lc = base.to_ascii_lowercase();
        // Subprogram/process locals live in names_stack, not names.
        match self.sig_id(base).or_else(|| self.stack_name_id(&base_lc)) {
            Some(id) => self.prot_member_expr(id, &last),
            None => self.pkg_prot_placeholder(base),
        }
    }

    /// Member of a protected object: parameterless method call, field read,
    /// or placeholder. `None` when the base is not a protected object.
    fn prot_member_expr(&self, id: SigId, last: &str) -> Option<Expr> {
        let t = self.sig_prot.get(&id).cloned()?;
        match self.prot_types.get(&t) {
            Some(pt) => {
                if pt.methods.iter().any(|m| m == last) {
                    return Some(Expr::Call {
                        name: format!("$method::{}::{}", t, last),
                        args: vec![Expr::Sig(id)],
                        shapes: Vec::new(),
                    });
                }
                if let Some((_, w, lsb, _)) = pt.fields.iter().find(|(n, _, _, _)| *n == last) {
                    return Some(Expr::Call {
                        name: format!("$getf::{}::{}@{},{}", t, last, lsb, w),
                        args: vec![Expr::Sig(id)],
                        shapes: Vec::new(),
                    });
                }
                // Unknown member on a known protected object.
                Some(Expr::Lit(Value::x(32)))
            }
            // Protected type from a use'd package (no
            // local body): member select is a placeholder.
            None => Some(Expr::Lit(Value::x(32))),
        }
    }

    /// Package-level shared variable of protected type:
    /// not simulated yet — placeholder. `None` keeps the fall-through.
    fn pkg_prot_placeholder(&self, base: &str) -> Option<Expr> {
        let tn = self.pkg_sig_types.get(base)?;
        matches!(self.types.get(tn), Some(TypeInfo::Handle)).then(|| Expr::Lit(Value::x(32)))
    }

    /// `arr(i).field` — the parser folds this into one Name node
    /// wrapping a Call plus `.field`. Lower the call, then select
    /// the record field inside the element (slice of the memory
    /// element for arrays of records). `None` falls through.
    fn indexed_field_select(
        &mut self,
        e: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) -> Option<Expr> {
        let ekids = real_children(e);
        let call = ekids
            .iter()
            .find(|k| k.kind() == SyntaxKind::Call)
            .cloned()?;
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
            return Some(base);
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
            Expr::Call { name, .. } if name == "$const_idx" => self.const_idx_elem_rec(&call),
            _ => None,
        };
        let Some(tn) = rec_name else {
            // Field select on an indexed/opaque record element
            // (`state.actors(0).subscribers`) — placeholder.
            return Some(Expr::Lit(Value::x(32)));
        };
        self.indexed_field_sel(base, &tn, &field)
    }

    /// `const_arr(i).field` lowered via `$const_idx`: the
    /// element record type comes from the constant's type.
    fn const_idx_elem_rec(&self, call: &SyntaxNode) -> Option<String> {
        let ck = real_children(call);
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

    /// Field select on the indexed element: first matching record field
    /// slices the element (first field = MSB). `None` when the type is not
    /// a record or the field is unknown — the caller falls through.
    fn indexed_field_sel(&self, base: Expr, tn: &str, field: &str) -> Option<Expr> {
        let Some(TypeInfo::Record { fields }) = self.types.get(tn) else {
            return None;
        };
        let total: usize = fields.iter().map(|f| f.1).sum();
        let mut run = 0usize;
        for (fn_, w, _) in fields {
            let lsb = total - run - w;
            if *fn_ == field {
                return Some(Expr::Sel {
                    e: Box::new(base),
                    msb: Box::new(Expr::Lit(Value::from_u64(32, (lsb + w - 1) as u64))),
                    lsb: Box::new(Expr::Lit(Value::from_u64(32, lsb as u64))),
                });
            }
            run += w;
        }
        None
    }

    /// Record field select cascade: signal root, record alias root,
    /// protected-state placeholder, package-signal placeholder, constant
    /// root. `None` falls through to plain resolution.
    fn record_field_select(&self, name: &str, env: &HashMap<String, Value>) -> Option<Expr> {
        let segs: Vec<&str> = name.split('.').collect();
        let base = segs[0].to_ascii_lowercase();
        let fields_path = &segs[1..];
        let id = self
            .sig_id(&base)
            .or_else(|| self.names.get(&format!("__const_{}", base)).copied())
            .or_else(|| self.stack_name_id(&base));
        if let Some(id) = id {
            if let Some(tn) = self.sig_rec.get(&id).cloned() {
                if let Some((lsb, w)) = self.field_path_offset(&tn, fields_path) {
                    return Some(lit_sel(Expr::Sig(id), lsb, w));
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
                if let Some((lsb, w)) = self.field_path_offset(&tn, fields_path) {
                    return Some(lit_sel(ae, lsb, w));
                }
            }
        }
        self.field_placeholder_or_const(&base, fields_path, env)
    }

    /// Lowercase the field path and resolve its bit offset in `tn`.
    fn field_path_offset(&self, tn: &str, fields_path: &[&str]) -> Option<(usize, usize)> {
        let path: Vec<String> = fields_path.iter().map(|s| s.to_ascii_lowercase()).collect();
        let path_refs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
        self.record_field_offset(tn, &path_refs)
    }

    /// The remaining dotted-name roots: protected-state member placeholder,
    /// package-level signal/shared-variable placeholder, and a record-
    /// typed constant/generic root (`rec_const.field` slices the literal).
    fn field_placeholder_or_const(
        &self,
        base: &str,
        fields_path: &[&str],
        env: &HashMap<String, Value>,
    ) -> Option<Expr> {
        let field = fields_path.join(".").to_ascii_lowercase();
        // Nested member select rooted at a protected-type state
        // variable (`st.ptr`, `st` a record-typed state field):
        // placeholder (not simulated).
        if env.get(base).is_none() {
            if let Some(t) = self.prot_body_ctx.clone() {
                if let Some(pt) = self.prot_types.get(&t) {
                    if pt.fields.iter().any(|(n, _, _, _)| *n == base) {
                        return Some(Expr::Lit(Value::x(32)));
                    }
                }
            }
        }
        // Package-level signal / shared variable: record-field or
        // protected-type member select lowers to a placeholder
        // (not simulated).
        if let Some(tn) = self.pkg_sig_types.get(base).cloned() {
            if let Some(TypeInfo::Record { fields }) = self.types.get(&tn) {
                if let Some((_, w, _)) = fields.iter().find(|(fn_, _, _)| *fn_ == field) {
                    return Some(Expr::Lit(Value::x((*w).max(1))));
                }
                return Some(Expr::Lit(Value::x(32)));
            }
        }
        if let (Some(v), Some(tn)) = (
            self.consts.get(base).cloned(),
            self.const_types.get(base).cloned(),
        ) {
            // `rec_const.field` (nested: `c.a.b`) on a record-typed
            // constant/generic: slice the literal value directly.
            if let Some((lsb, w)) = self.field_path_offset(&tn, fields_path) {
                return Some(lit_sel(Expr::Lit(v), lsb, w));
            }
        }
        None
    }

    /// Plain identifier resolution: inner scope (env: loop vars, params),
    /// signals, constants, aliases, parameterless function references,
    /// special names, package items, physical units, protected/package
    /// placeholders, then the unknown-identifier diagnostic.
    fn resolve_plain_name(
        &mut self,
        e: &SyntaxNode,
        name: &str,
        env: &HashMap<String, Value>,
    ) -> Expr {
        let base = name.rsplit('.').next().unwrap_or(name);
        let key = base.to_ascii_lowercase();
        if let Some(ex) = self.scope_name(base, &key, env) {
            return ex;
        }
        if let Some(ex) = self.special_name(base, &key) {
            return ex;
        }
        if let Some(ex) = self.pkg_item_select(name) {
            return ex;
        }
        if let Some(ex) = self.unit_literal(&key) {
            return ex;
        }
        if let Some(ex) = self.prot_or_pkg_placeholder(&key) {
            return ex;
        }
        self.err(e.span(), format!("unknown identifier '{}'", base));
        Expr::Lit(Value::x(1))
    }

    /// Local scope (env: loop vars, params) first, then signals, then
    /// constants — so an enum literal from a `use`d package (e.g. 'U'
    /// from std_logic_1164) cannot shadow a signal named `u`.
    /// Inside a function body `names` holds exactly the
    /// formals/locals, so a formal shadowing an outer constant that
    /// reached the env snapshot wins (LRM 10.3 inner visibility):
    /// an architecture constant `C_DATA` must not override a
    /// formal `C_DATA` in the body.
    fn scope_name(&self, base: &str, key: &str, env: &HashMap<String, Value>) -> Option<Expr> {
        if let Some(v) = env.get(key) {
            // Inside a function body `names` holds exactly the
            // formals/locals, so a formal shadowing an outer constant
            // that reached the env snapshot wins (LRM 10.3 inner
            // visibility): an architecture constant `C_DATA` must not
            // override a formal `C_DATA` in the body. `sig_id` is
            // case-insensitive; `names` keys keep the source case.
            if self.fn_scope == 0 || self.sig_id(base).is_none() {
                return Some(self.const_use(key, v.clone()));
            }
        }
        if let Some(id) = self.sig_id(base) {
            return Some(Expr::Sig(id));
        }
        if let Some(v) = self.consts.get(key) {
            return Some(self.const_use(key, v.clone()));
        }
        if let Some(a) = self.aliases.get(key) {
            return Some(a.clone());
        }
        self.parameterless_call(key)
    }

    /// Function referenced by name (parameterless call `c_hi`, or an
    /// alias target `alias a is f[...];`): resolve to a call. When
    /// every formal carries a recorded default, the bare name is a
    /// legal parameterless call — emit no actuals, the omitted
    /// formals take their defaults. Otherwise placeholder actuals
    /// keep the reference a call (parameterized bare names are only
    /// legal as alias targets; they read X).
    fn parameterless_call(&self, key: &str) -> Option<Expr> {
        let f = self.funcs.get(key).and_then(|c| c.first())?;
        let n_actuals = if !f.defaults.is_empty() && f.defaults.iter().all(|d| d.is_some()) {
            0
        } else {
            f.args.len()
        };
        Some(Expr::Call {
            name: f.name.clone(),
            args: vec![Expr::Lit(Value::x(32)); n_actuals],
            shapes: Vec::new(),
        })
    }

    /// Special names after scope resolution: `open`, type names as values,
    /// `now`, Xilinx implicit globals, and the enclosing function scope.
    fn special_name(&mut self, base: &str, key: &str) -> Option<Expr> {
        // `open` (reserved word) in a port/generic map association:
        // an unconnected port — X, no diagnostic.
        if key == "open" {
            return Some(Expr::Lit(Value::x(1)));
        }
        // A type name used as a value (`alias t2 is t1;`) — placeholder.
        if self.types.contains_key(key) {
            return Some(Expr::Lit(Value::x(32)));
        }
        // std.standard `now` — current simulation time (sim: `$now`).
        if key == "now" {
            return Some(Expr::Call {
                name: "$now".into(),
                args: Vec::new(),
                shapes: Vec::new(),
            });
        }
        // Xilinx implicit globals (unisim references bare `GSR` etc.).
        if matches!(key, "gsr" | "grestore" | "gts") {
            return Some(Expr::Sig(self.ensure_global_sig(base)));
        }
        // Enclosing function scope (nested functions see outer
        // parameters/locals).
        self.stack_name_id(key).map(Expr::Sig)
    }

    /// Case-insensitive lookup in the enclosing subprogram/process
    /// scope stack.
    fn stack_name_id(&self, key: &str) -> Option<SigId> {
        self.names_stack.iter().rev().find_map(|m| {
            m.get(key).copied().or_else(|| {
                m.iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(key))
                    .map(|(_, v)| *v)
            })
        })
    }

    /// 2-part `pkg.item` and 3+ part `lib.pkg.item` selected names:
    /// resolve the item inside the named package.
    fn pkg_item_select(&mut self, name: &str) -> Option<Expr> {
        let parts: Vec<&str> = name.split('.').collect();
        if parts.len() < 2 {
            return None;
        }
        let pkg = if parts.len() >= 3 {
            parts[parts.len() - 2].to_ascii_lowercase()
        } else {
            parts[0].to_ascii_lowercase()
        };
        let item = parts[parts.len() - 1].to_ascii_lowercase();
        if let Some(p) = self.pkgs.get(&pkg).cloned() {
            if let Some(ex) = self.pkg_item_expr(p, &pkg, &item) {
                return Some(ex);
            }
        }
        // A package used as a value (package actual in a generic
        // map: `generic map (P => work.my_pkg)`) — inert placeholder.
        self.pkgs
            .contains_key(&item)
            .then(|| Expr::Lit(Value::x(32)))
    }

    /// One item of a resolved package: constant, parameterless function,
    /// type (placeholder), or package-level signal materialized under the
    /// qualified name (`Manager.Cyc`, `Subordinate.Adr`) so two instances
    /// of the same generic package stay distinct. `None` when the item
    /// matches nothing.
    fn pkg_item_expr(&mut self, p: Rc<PackageIr>, pkg: &str, item: &str) -> Option<Expr> {
        if let Some((_, v)) = p.consts.iter().find(|(n, _)| *n == item) {
            return Some(Expr::Lit(v.clone()));
        }
        if let Some(f) = p
            .functions
            .iter()
            .find(|f| f.name.to_ascii_lowercase() == item)
        {
            return Some(Expr::Call {
                name: f.name.clone(),
                args: vec![Expr::Lit(Value::x(32)); f.args.len()],
                shapes: Vec::new(),
            });
        }
        if p.types.iter().any(|(n, _)| *n == item) {
            return Some(Expr::Lit(Value::x(32)));
        }
        let tn = p
            .sig_types
            .iter()
            .find(|(n, _)| *n == item)
            .map(|(_, tn)| tn.clone())?;
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
        Some(Expr::Sig(id))
    }

    /// Bare physical unit name (`ns`, `ps`, …) as a literal of one
    /// unit, in femtoseconds; user-declared units (`mm`, `cm`,
    /// `angstrom`, ...) in base units.
    fn unit_literal(&self, key: &str) -> Option<Expr> {
        if matches!(
            key,
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
            return Some(Expr::Lit(Value::from_u64(64, time_unit_fs(key))));
        }
        self.phys_units
            .get(key)
            .copied()
            .map(|v| Expr::Lit(Value::from_u64(64, v)))
    }

    /// Protected/package tail of plain resolution: a bare state-variable
    /// reference inside a protected-type body, a package-level shared
    /// variable, and the protected-body catch-all placeholder.
    fn prot_or_pkg_placeholder(&self, key: &str) -> Option<Expr> {
        // Bare state-variable reference inside a protected-type body:
        // read the field of `$this` (`$getf::t::field@lsb,w`).
        if let Some(t) = self.prot_body_ctx.clone() {
            if let Some(pt) = self.prot_types.get(&t) {
                if let Some((_, w, lsb, _)) = pt.fields.iter().find(|(n, _, _, _)| *n == key) {
                    return Some(Expr::Call {
                        name: format!("$getf::{}::{}@{},{}", t, key, lsb, w),
                        args: vec![Expr::Call {
                            name: "$this".into(),
                            args: vec![],
                            shapes: Vec::new(),
                        }],
                        shapes: Vec::new(),
                    });
                }
            }
        }
        // Package-level shared variable read from a package-body
        // subprogram: placeholder (not simulated).
        if self.pkg_sig_types.contains_key(key) {
            return Some(Expr::Lit(Value::x(1)));
        }
        // Inside a protected-type body, unresolved names are common
        // (conversions to the protected object, use'd package items,
        // access-type fields): placeholder, no diagnostic.
        self.prot_body_ctx
            .is_some()
            .then(|| Expr::Lit(Value::x(32)))
    }
}

/// VHDL-2008 `.all` dereference of an access object: lower the
/// prefix (`Buf.all` -> `Buf`); a mid-chain `.all` dereference:
/// `res.all.chain` -> `res.chain`.
fn strip_all_derefs(text: String) -> String {
    let stripped = if text.len() > 4
        && text.as_bytes()[text.len() - 4] == b'.'
        && text[text.len() - 3..].eq_ignore_ascii_case("all")
    {
        text[..text.len() - 4].to_string()
    } else {
        text
    };
    stripped
        .split('.')
        .filter(|s| !s.eq_ignore_ascii_case("all"))
        .collect::<Vec<_>>()
        .join(".")
}

/// Field slice of a record carrier (first field = MSB): bit range
/// [lsb+w-1 … lsb] of `e`.
fn lit_sel(e: Expr, lsb: usize, w: usize) -> Expr {
    Expr::Sel {
        e: Box::new(e),
        msb: Box::new(Expr::Lit(Value::from_u64(32, (lsb + w - 1) as u64))),
        lsb: Box::new(Expr::Lit(Value::from_u64(32, lsb as u64))),
    }
}
