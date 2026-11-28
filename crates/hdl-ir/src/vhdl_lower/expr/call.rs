//! VHDL call lowering: `f(args)` in expression position. The callee
//! prelude (attribute callees, name/argument split), the sequential
//! early-return routes (const-rooted selected chains, `.all(i)` deref,
//! dotted callees, package-scope merges, constant-array element reads,
//! protected-object methods), and the fname dispatch (signal indexing,
//! shifts, conversions, resizes, operator symbols, aliases) — split out
//! of `expr.rs` to keep every function at its standards baseline.

use super::*;

impl<'a> Lower<'a> {
    pub(super) fn lower_call(&mut self, e: &SyntaxNode, env: &HashMap<String, Value>) -> Expr {
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
        let (name, fname, arg_nodes, args) = self.call_callee_args(&kids, attr_pos, env);
        self.route_call(e, env, name, fname, arg_nodes, args)
    }

    /// The callee name, its last dotted segment, the argument nodes, and
    /// the lowered arguments. The callee is the first Name child (absent
    /// for attribute calls like `integer'image(x)`, where kids are
    /// [Attr, args...]); arguments are every other expression child —
    /// plain names included (`rising_edge(clk)`).
    fn call_callee_args(
        &mut self,
        kids: &[SyntaxNode],
        attr_pos: Option<usize>,
        env: &HashMap<String, Value>,
    ) -> (String, String, Vec<SyntaxNode>, Vec<Expr>) {
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
        (name, fname, arg_nodes, args)
    }

    /// The sequential early-return routes after the callee/argument
    /// prelude, then the fname dispatch.
    fn route_call(
        &mut self,
        e: &SyntaxNode,
        env: &HashMap<String, Value>,
        name: String,
        fname: String,
        arg_nodes: Vec<SyntaxNode>,
        args: Vec<Expr>,
    ) -> Expr {
        if let Some(ex) = self.const_chain_call(e, env, &name) {
            return ex;
        }
        if let Some(ex) = self.all_deref_index(&name, &fname, &args) {
            return ex;
        }
        let base_id = self.base_signal_id(&name, &fname);
        if base_id.is_none() && name.contains('.') {
            if let Some(ex) = self.dotted_callee_call(&name, &fname, &args) {
                return ex;
            }
        }
        self.merge_pkg_scope(&name, &fname);
        if base_id.is_none() && !args.is_empty() {
            if let Some(e) = self.lower_const_array_index(&fname, &args) {
                return e;
            }
        }
        if let Some(ex) = self.prot_member_call(&name, &args) {
            return ex;
        }
        if let Some(ex) = self.prot_self_call(&fname, &args) {
            return ex;
        }
        self.dispatch_call(base_id, fname, args, &arg_nodes, env)
    }

    /// `c_vec(0).f(0)` — a selected-name chain as the callee: the parser
    /// folds the prefix into one dotted Name, so only the call path sees
    /// it. Route const-rooted chains through the chain walker (it rejects
    /// function roots), which interleaves field and index segments over
    /// the constant's flat value — the same expression the runtime and
    /// const-eval already fold. Signal/alias roots keep their existing
    /// per-shape handling below.
    fn const_chain_call(
        &mut self,
        e: &SyntaxNode,
        env: &HashMap<String, Value>,
        name: &str,
    ) -> Option<Expr> {
        if !name.contains('.') {
            return None;
        }
        let root = name
            .split(['.', '('])
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if root.is_empty() || !self.consts.contains_key(&root) || self.funcs.contains_key(&root) {
            return None;
        }
        self.lower_selected_chain(e, env)
    }

    /// `.all(i)` dereference of an access-to-array: `L.all(i)` -> L(i).
    fn all_deref_index(&self, name: &str, fname: &str, args: &[Expr]) -> Option<Expr> {
        if fname != "all"
            || name.len() <= 4
            || name.as_bytes()[name.len() - 4] != b'.'
            || args.is_empty()
        {
            return None;
        }
        let prefix = &name[..name.len() - 4];
        let id = self.sig_id(prefix)?;
        Some(Expr::Idx {
            e: Box::new(Expr::Sig(id)),
            i: Box::new(args[0].clone()),
        })
    }

    /// A record/array-typed constant element read routes through
    /// `lower_const_array_index` below (`$const_idx` over the literal —
    /// the const-eval cannot see a materialized hidden signal).
    fn base_signal_id(&self, name: &str, fname: &str) -> Option<SigId> {
        self.sig_id(name).or_else(|| match self.aliases.get(fname) {
            Some(Expr::Sig(id)) => Some(*id),
            _ => None,
        })
    }

    /// Dotted callee `rec.field(args)` or `pkg_var.method(args)`: if the
    /// base names a signal/record or a package-level signal, this is a
    /// method call — lower as a plain call of the field name.
    fn dotted_callee_call(&mut self, name: &str, fname: &str, args: &[Expr]) -> Option<Expr> {
        let base = name.split('.').next().unwrap_or(name);
        // `rec.field(i)` where `field` is an array-typed field of a
        // record signal: index the field's slice. Emitting the method
        // call fallback here would call a function named after the
        // field (→ X at runtime) and drop the select entirely.
        if args.len() == 1 {
            if let Some(ex) = self.dotted_field_index(base, fname, &args[0]) {
                return Some(ex);
            }
        }
        if self.sig_id(base).is_some()
            || self.pkg_sig_types.contains_key(&base.to_ascii_lowercase())
        {
            return Some(Expr::Call {
                name: fname.to_string(),
                args: args.to_vec(),
                shapes: Vec::new(),
            });
        }
        None
    }

    /// `rec.field(i)` on a record-typed signal: index into the field's
    /// slice of the flat record vector (first field = MSB). `None` falls
    /// through to the method-call fallback.
    fn dotted_field_index(&self, base: &str, fname: &str, k: &Expr) -> Option<Expr> {
        let id = self.sig_id(base)?;
        let tn = self.sig_rec.get(&id).cloned()?;
        let Some(TypeInfo::Record { fields }) = self.types.get(&tn) else {
            return None;
        };
        let fl = fname.to_ascii_lowercase();
        let total: usize = fields.iter().map(|f| f.1).sum();
        let mut run = 0usize;
        for (fn_, w, _) in fields {
            let lsb = total - run - w;
            if fn_.to_ascii_lowercase() == fl {
                let sel = Expr::Sel {
                    e: Box::new(Expr::Sig(id)),
                    msb: Box::new(Expr::Lit(Value::from_u64(32, (lsb + w - 1) as u64))),
                    lsb: Box::new(Expr::Lit(Value::from_u64(32, lsb as u64))),
                };
                return Some(Expr::Idx {
                    e: Box::new(sel),
                    i: Box::new(k.clone()),
                });
            }
            run += w;
        }
        None
    }

    /// Fully-qualified call `work.pkg.f(...)` with no `use` clause for pkg:
    /// pull the package's subprogram into scope so the call resolves (and
    /// can be const-evaluated in a constant initializer).
    fn merge_pkg_scope(&mut self, name: &str, fname: &str) {
        if self.funcs.contains_key(fname) || !name.contains('.') {
            return;
        }
        let parts: Vec<&str> = name.split('.').collect();
        if parts.len() < 3 {
            return;
        }
        let unit = parts[parts.len() - 2].to_ascii_lowercase();
        if let Some(p) = self.pkgs.get(&unit).cloned() {
            self.merge_package(&p, Some(fname), false);
        }
    }

    /// `obj.m(args)` on a protected-type object → `$method::t::m(obj, …)`.
    /// `None` falls through to the remaining routes.
    fn prot_member_call(&mut self, name: &str, args: &[Expr]) -> Option<Expr> {
        let dot = name.find('.')?;
        let (base, m) = (&name[..dot], name[dot + 1..].to_ascii_lowercase());
        if let Some(id) = self.sig_id(base) {
            let t = self.sig_prot.get(&id).cloned()?;
            let is_method = self
                .prot_types
                .get(&t)
                .is_some_and(|p| p.methods.contains(&m));
            if is_method {
                let mut args2 = vec![Expr::Sig(id)];
                args2.extend(args.iter().cloned());
                return Some(Expr::Call {
                    name: format!("$method::{}::{}", t, m),
                    args: args2,
                    shapes: Vec::new(),
                });
            }
            // Unknown member on a protected object — or any member of
            // a protected type from a use'd package: placeholder.
            return Some(Expr::Lit(Value::x(32)));
        }
        if let Some(tn) = self.pkg_sig_types.get(base) {
            // Package-level shared variable of protected type: placeholder.
            return matches!(self.types.get(tn), Some(TypeInfo::Handle))
                .then(|| Expr::Lit(Value::x(32)));
        }
        let t = self.prot_body_ctx.clone()?;
        // `st.field(args)` rooted at a protected-type state variable:
        // placeholder (not simulated).
        self.prot_types
            .get(&t)
            .is_some_and(|pt| {
                pt.fields
                    .iter()
                    .any(|(n, _, _, _)| n.eq_ignore_ascii_case(base))
            })
            .then(|| Expr::Lit(Value::x(32)))
    }

    /// Bare `m(args)` inside a protected-type body → self-call on `$this`.
    fn prot_self_call(&mut self, fname: &str, args: &[Expr]) -> Option<Expr> {
        let t = self.prot_body_ctx.clone()?;
        if !self
            .prot_types
            .get(&t)
            .is_some_and(|p| p.methods.iter().any(|m| m == fname))
        {
            return None;
        }
        let mut args2 = vec![Expr::Call {
            name: "$this".into(),
            args: vec![],
            shapes: Vec::new(),
        }];
        args2.extend(args.iter().cloned());
        Some(Expr::Call {
            name: format!("$method::{}::{}", t, fname),
            args: args2,
            shapes: Vec::new(),
        })
    }
}
