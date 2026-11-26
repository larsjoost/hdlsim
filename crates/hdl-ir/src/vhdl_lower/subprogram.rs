//! Subprogram lowering: function/procedure declarations and stubs, dynamic-width local machinery (dyn_width_expr/dyn_others_init), and the lower_function_node body lowerer.
//!
//! Extracted verbatim from vhdl_lower.rs (pure move, no logic changes).

use super::slots::FnLocalDecls;
use super::*;

impl<'a> Lower<'a> {
    /// Lower an architecture-level function declaration to FuncIr. The body is
    /// lowered in slot space: slot 0 = function name (return value), then
    /// parameters, then local variables.
    pub(super) fn lower_function_decl(&mut self, module: &mut ModuleIr, f: &SyntaxNode) {
        if let Some(fi) = self.lower_function_node(f) {
            self.register_func(fi.clone());
            module.functions.push(fi);
        }
    }

    /// Whether a `SyntaxKind::Function` node (function or procedure
    /// declaration) is a true function, i.e. has a `return` before `begin`.
    /// Process-local procedures are not lowered: their bodies reference
    /// outer-scope names (ports, process aliases) that the subprogram slot
    /// space does not see.
    pub(super) fn subprogram_is_function(&self, f: &SyntaxNode) -> bool {
        for k in real_children(f) {
            if k.kind() == SyntaxKind::Ident {
                match self.kw(&k).as_str() {
                    "return" => return true,
                    "begin" => return false,
                    _ => {}
                }
            }
        }
        false
    }

    /// Name + argument count of a subprogram declaration, as a stub FuncIr —
    /// pre-registered so declarative items (constant initializers) that
    /// reference a nested subprogram before its body is lowered resolve to a
    /// call; the real body overwrites the stub afterwards.
    pub(super) fn subprogram_stub(&self, f: &SyntaxNode) -> Option<FuncIr> {
        let kids = real_children(f);
        let mut name = String::new();
        let mut seen_return = false;
        let mut seen_begin = false;
        let mut nargs = 0usize;
        for k in &kids {
            match k.kind() {
                SyntaxKind::Ident if self.kw(k) == "return" => seen_return = true,
                SyntaxKind::Ident if self.kw(k) == "begin" => seen_begin = true,
                SyntaxKind::Name if !seen_return && name.is_empty() => name = self.text(k),
                SyntaxKind::PortDecl if !seen_begin => {
                    let pk = real_children(k);
                    if let Some(colon) = pk
                        .iter()
                        .position(|c| c.kind() == SyntaxKind::Punct && self.text(c) == ":")
                    {
                        nargs += pk[..colon]
                            .iter()
                            .filter(|c| c.kind() == SyntaxKind::Name)
                            .count();
                    }
                }
                _ => {}
            }
        }
        if name.is_empty() {
            return None;
        }
        Some(FuncIr {
            name,
            ret_width: 32,
            args: (0..nargs)
                .map(|i| (format!("a{i}"), 32, String::new()))
                .collect(),
            defaults: Vec::new(),
            locals: Vec::new(),
            dyn_locals: Vec::new(),
            body: Stmt::Null,
        })
    }

    /// Bit-width expression for a local's formal-dependent index constraint:
    /// `string(1 to size)` → `size * 8`, `bit_vector(size-1 downto 0)` →
    /// `size` (element width follows `resolve_type`: `string` packs 8 bits
    /// per character, other vectors 1). Direction decides which bound is
    /// high; a null or reversed range still yields the (hi - lo + 1) shape
    /// the runtime assign would produce.
    pub(super) fn dyn_width_expr(&mut self, tm: &SyntaxNode, c: &SyntaxNode) -> Expr {
        let tn = self
            .kw(tm)
            .rsplit('.')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        // Element width follows the declared type: `string` packs 8-bit
        // characters, a declared array's `TypeInfo::Array.elem` carries its
        // element width (an `integer` array element is 32 bits), unconstrained
        // vectors are bit arrays.
        let elem = self.array_elem_width(&tn).unwrap_or(1);
        // Descend into the nested Range of a parenthesized group; a
        // multi-dimensional dynamic constraint falls back to X (rare).
        let subs: Vec<SyntaxNode> = real_children(c)
            .into_iter()
            .filter(|k| k.kind() == SyntaxKind::Range)
            .collect();
        let r = match subs.len() {
            1 => &subs[0],
            0 => c,
            _ => return Expr::Lit(Value::x(32)),
        };
        // `variable x : real_array_t(coefficients'range)`: an attribute
        // range names no bounds — the local spans the object's own extent.
        // Its bit width is the actual's bit width (`SigAttr length`): same
        // element width, so count x elem == the object's bits. A type-mark
        // range folds statically and never reaches here.
        let expr_nodes: Vec<SyntaxNode> = real_children(r)
            .into_iter()
            .filter(|k| is_expr_node(k.kind()))
            .collect();
        if expr_nodes.len() == 1 {
            let raw = self.text(&expr_nodes[0]);
            let tl = raw.to_ascii_lowercase();
            let attr_len = if tl.ends_with("'reverse_range") {
                "'reverse_range".len()
            } else if tl.ends_with("'range") {
                "'range".len()
            } else {
                0
            };
            if attr_len > 0 {
                let base = raw[..raw.len() - attr_len].trim();
                if let Some(id) = self.sig_id(base) {
                    return Expr::SigAttr {
                        sig: id,
                        attr: "length",
                    };
                }
            }
        }
        let env = self.consts.clone();
        let exprs: Vec<Expr> = real_children(r)
            .into_iter()
            .filter(|k| is_expr_node(k.kind()))
            .map(|k| self.lower_expr(&k, &env))
            .collect();
        let (hi, lo) = match exprs.len() {
            2 if self.text(r).to_ascii_lowercase().contains("downto") => {
                (exprs[0].clone(), exprs[1].clone())
            }
            2 => (exprs[1].clone(), exprs[0].clone()),
            1 => (exprs[0].clone(), Expr::Lit(Value::from_u64(32, 1))),
            _ => return Expr::Lit(Value::x(32)),
        };
        let count = Expr::Bin {
            op: BinOp::Add,
            l: Box::new(Expr::Bin {
                op: BinOp::Sub,
                l: Box::new(hi),
                r: Box::new(lo),
            }),
            r: Box::new(Expr::Lit(Value::from_u64(32, 1))),
        };
        Expr::Bin {
            op: BinOp::Mul,
            l: Box::new(count),
            r: Box::new(Expr::Lit(Value::from_u64(32, elem as u64))),
        }
    }

    /// Re-emit an OTHERS aggregate initializer for a dynamic-width local:
    /// `Replicate { count: width_expr / element_bits, e: value }` — the
    /// initializer folds to the per-call constraint width instead of the
    /// registration fallback. `None` keeps the generic lowering (non-OTHERS
    /// aggregates, already-lowered inits).
    pub(super) fn dyn_others_init(&mut self, node: &SyntaxNode, wexpr: &Expr) -> Option<Expr> {
        if node.kind() != SyntaxKind::Aggregate {
            return None;
        }
        let kids = real_children(node);
        if !kids.iter().any(|k| k.kind() == SyntaxKind::Others) {
            return None;
        }
        let val = kids.iter().rev().find(|k| is_expr_node(k.kind()))?;
        let inner = self.lower_expr(val, &self.consts.clone());
        let iw = match &inner {
            Expr::Lit(v) => v.width().max(1),
            _ => 1,
        };
        Some(Expr::Replicate {
            count: Box::new(Expr::Bin {
                op: BinOp::Div,
                l: Box::new(wexpr.clone()),
                r: Box::new(Expr::Lit(Value::from_u64(32, iw as u64))),
            }),
            e: Box::new(inner),
        })
    }

    /// VHDL-2019 `return rv_t of elem_t`: register the return type mark as
    /// an alias of an array of the `of` element type, so the mark resolves
    /// when the formal loop reads it. A missing element type falls back to
    /// the unsigned-vector default.
    fn register_of_return_alias(
        &mut self,
        ret_mark: &Option<SyntaxNode>,
        of_mark: &Option<SyntaxNode>,
    ) {
        if let (Some(rm), Some(om)) = (ret_mark, of_mark) {
            let rn = self.kw(rm).to_ascii_lowercase();
            let on = self.kw(om).to_ascii_lowercase();
            let ti = self
                .types
                .get(&on)
                .cloned()
                .unwrap_or(TypeInfo::Vector { signed: false });
            self.types.insert(rn, ti);
        }
    }

    /// Split a subprogram's CST into its syntactic parts, registering a
    /// VHDL-2019 subprogram-generic clause if present. `None` for a
    /// nameless (malformed) subprogram.
    fn split_subprogram_header(&mut self, f: &SyntaxNode) -> Option<SubprogramHeader> {
        let kids = real_children(f);
        let mut name = String::new();
        let mut ret_mark: Option<SyntaxNode> = None;
        let mut of_mark: Option<SyntaxNode> = None;
        let mut decl_nodes: Vec<SyntaxNode> = Vec::new();
        let mut nested_fns: Vec<SyntaxNode> = Vec::new();
        let mut stmt_nodes: Vec<SyntaxNode> = Vec::new();
        let mut seen_return = false;
        let mut seen_of = false;
        let mut seen_begin = false;
        for k in &kids {
            match k.kind() {
                SyntaxKind::Ident if self.kw(k) == "return" => seen_return = true,
                SyntaxKind::Ident if self.kw(k) == "of" && seen_return => seen_of = true,
                SyntaxKind::Ident if self.kw(k) == "begin" => seen_begin = true,
                // VHDL-2019 subprogram generic clause (`function f generic
                // (type T) parameter ...`): register its type formals so the
                // parameter list (`a : T`, `b : a'subtype`) resolves.
                SyntaxKind::GenericClause if !seen_begin => {
                    let _ = self.lower_generic_decls(k, &HashMap::new());
                }
                SyntaxKind::Name if !seen_return && name.is_empty() => name = self.text(k),
                SyntaxKind::Name if seen_return && !seen_begin && ret_mark.is_none() => {
                    ret_mark = Some(k.clone())
                }
                // VHDL-2019 `return rv_t of std_logic_vector`: the `of` clause
                // names the element type; the return type mark is an alias of
                // an array of it (register before resolving the mark below).
                SyntaxKind::Name if seen_of && !seen_begin && of_mark.is_none() => {
                    of_mark = Some(k.clone())
                }
                // Declarative items in textual order: each may reference the
                // ones before it (params, types, constants, locals).
                _ if !seen_begin && is_decl_item(k.kind()) => decl_nodes.push(k.clone()),
                // Nested function declarations (`impure function f return t is
                // ...`) — lowered after the outer slot map is installed below,
                // so outer parameters/locals are visible via the names stack.
                SyntaxKind::Function if !seen_begin => nested_fns.push(k.clone()),
                _ if seen_begin && is_stmt_node(k.kind()) => stmt_nodes.push(k.clone()),
                _ => {}
            }
        }
        if name.is_empty() {
            return None;
        }
        self.register_of_return_alias(&ret_mark, &of_mark);
        Some(SubprogramHeader {
            name,
            ret_mark,
            decl_nodes,
            nested_fns,
            stmt_nodes,
        })
    }

    /// Split one PortDecl interface into its formals' names, type mark, and
    /// optional range constraint.
    fn formal_parts(
        &self,
        pd: &SyntaxNode,
    ) -> (Vec<String>, Option<SyntaxNode>, Option<SyntaxNode>) {
        let pk = real_children(pd);
        let colon = pk
            .iter()
            .position(|k| k.kind() == SyntaxKind::Punct && self.text(k) == ":");
        let names: Vec<String> = pk[..colon.unwrap_or(pk.len())]
            .iter()
            .filter(|k| k.kind() == SyntaxKind::Name)
            .map(|k| self.text(k))
            .collect();
        // Type mark is the first Name after `:` — a mode keyword
        // (`inout`, `out`, …) or a `view V of` prefix may sit between.
        let tmark = colon.and_then(|c| self.iface_type_mark(&pk[c + 1..]).cloned());
        let constraint = tmark
            .as_ref()
            .and_then(|tm| {
                pk.iter()
                    .position(|k| std::ptr::eq(k, tm))
                    .and_then(|i| pk.get(i + 1))
            })
            .filter(|k| k.kind() == SyntaxKind::Range)
            .cloned();
        (names, tmark, constraint)
    }

    /// Interface default (`a, b : natural := 3`): const-eval now and record
    /// one entry per formal of this decl — a call may omit the
    /// corresponding actual. Defaults are static expressions; one
    /// referencing a formal, a later declaration, or a name from a
    /// package hdlsim does not have (ieee.fixed_pkg formal styles) stays
    /// unrecorded (an omitted actual then reads X, as before this feature).
    /// Lower leniently: a diagnostic here would fail files that never omit
    /// these actuals. `:=` lexes as the `=` punct (same as declaration
    /// initializers).
    fn iface_default(&mut self, pk: &[SyntaxNode], w: usize) -> Option<Value> {
        pk.iter()
            .position(|k| k.kind() == SyntaxKind::Punct && self.text(k) == "=")
            .and_then(|eq| {
                pk[eq + 1..]
                    .iter()
                    .find(|k| is_expr_node(k.kind()))
                    .cloned()
            })
            .map(|e| {
                self.suppress_err += 1;
                let ex = self.lower_expr(&e, &self.consts.clone());
                self.suppress_err -= 1;
                ex
            })
            .and_then(|ex| self.ce(&ex))
            .map(|v| v.resize(w.max(1)))
    }

    /// Collect a subprogram's interface from its PortDecl declarative
    /// items: per-formal slot width/type/shapes, interface defaults, and
    /// the return width. The return mark resolves after the loop so a
    /// `return a'subtype` mark (base = an earlier formal) finds its width.
    fn lower_formal_params(
        &mut self,
        decl_nodes: &[SyntaxNode],
        ret_mark: &Option<SyntaxNode>,
        f: &SyntaxNode,
    ) -> FormalParams {
        // Params: PortDecl `a, b : integer [range] [:= default]` — names
        // before ':', type after.
        let mut fp = FormalParams {
            param_w: HashMap::new(),
            args: Vec::new(),
            arg_types: Vec::new(),
            arg_shapes: Vec::new(),
            defaults: Vec::new(),
            ret_width: 32,
        };
        for pd in decl_nodes
            .iter()
            .filter(|d| d.kind() == SyntaxKind::PortDecl)
        {
            let pk = real_children(pd);
            let (names, tmark, constraint) = self.formal_parts(pd);
            // `b : a'subtype` — the base is an earlier formal of this
            // subprogram (VHDL-2019 subprogram generics); adopt its width.
            let (w, _, _, _) = match &tmark {
                Some(tm) if fp.param_w.contains_key(&self.kw(tm)) => {
                    (fp.param_w[&self.kw(tm)], false, None, false)
                }
                Some(tm) => self.resolve_type(tm, constraint.as_ref(), pd.span()),
                None => (32, false, None, false),
            };
            // The declared type name disambiguates same-width overloads at
            // dispatch (see FlatFunc::formal_types).
            let ty = tmark
                .as_ref()
                .map(|tm| self.text(tm).to_ascii_lowercase())
                .unwrap_or_default();
            let default_v = self.iface_default(&pk, w);
            for n in names {
                fp.param_w.insert(n.to_ascii_lowercase(), w);
                if let Some(tm) = &tmark {
                    fp.arg_types.push((n.clone(), self.kw(tm)));
                }
                // Per-formal type mark + constraint for `record_obj_shape` —
                // the formal's element width (`string` → 8) and index
                // direction must be known while the body lowers
                // (`arg'length`, slices over locals).
                fp.arg_shapes
                    .push((n.clone(), tmark.clone(), constraint.clone()));
                fp.args.push((n, w, ty.clone()));
                fp.defaults.push(default_v.clone());
            }
        }
        if let Some(tm) = ret_mark {
            fp.ret_width = match fp.param_w.get(&self.kw(tm)) {
                Some(w) => *w,
                None => self.resolve_type(tm, None, f.span()).0,
            };
        }
        fp
    }

    /// Map a slot's declared type name to its record/elem-record/protected
    /// entries, so `x.field` member selects resolve over formals and locals
    /// alike. Only names present in `types` get entries; a slot whose type
    /// is neither record, array-of-record, nor protected is left untouched.
    pub(super) fn map_slot_records(
        &mut self,
        slots: &[(String, usize)],
        types: &[(String, String)],
    ) {
        for (i, (n, _)) in slots.iter().enumerate() {
            let tn = types
                .iter()
                .find(|(an, _)| an == n)
                .map(|(_, tn)| tn.clone());
            if let Some(tn) = tn {
                if let Some(rn) = self.as_record(&tn) {
                    self.sig_rec.insert(i as SigId, rn);
                }
                if let Some(en) = self.as_elem_record(&tn) {
                    self.sig_elem_rec.insert(i as SigId, en);
                }
                // Protected-type object local: member selects placeholder.
                if matches!(self.types.get(&tn), Some(TypeInfo::Handle)) {
                    self.sig_prot.insert(i as SigId, tn.clone());
                }
            }
        }
    }

    /// Resolve a subprogram local's declared width: `resolve_type`, then an
    /// unconstrained array mark with a static constraint
    /// (`register_definition_array_t(0 to 63)`) multiplies in the element
    /// count — resolve_type returns the bare element width, but the local's
    /// flat slot covers every element the constraint adds. Subprogram locals
    /// are flat values, never mems. A dynamic constraint keeps the fallback
    /// width and returns a recorded `dyn_w` instead: a constraint that
    /// references subprogram formals (`variable s : string(1 to size)`)
    /// can't fold at registration — formals have no values yet — so the
    /// recorded width is a fallback and the expression is const-evaled per
    /// call. Only array-like types qualify for `dyn_w`: a record's flat
    /// width is the sum of its fields, and a record-element constraint
    /// (`t_node(name(1 to n))`) selects field ranges — lowering its element
    /// name as an identifier would misfire E0401.
    fn resolve_fn_local_width(
        &mut self,
        tmark: Option<&SyntaxNode>,
        constraint: Option<&SyntaxNode>,
        span: Span,
    ) -> (usize, bool, bool, Option<Expr>) {
        let (w, signed, _, real) = match tmark {
            Some(tm) => self.resolve_type(tm, constraint, span),
            None => (32, false, None, false),
        };
        let w = match (tmark, constraint) {
            (Some(tm), Some(c))
                if matches!(
                    self.types.get(&self.kw(tm)),
                    Some(TypeInfo::Array { dims, .. }) if dims.is_empty()
                ) && !self.text(c).contains("<>")
                    && self.constraint_folds(c) =>
            {
                w.max(1) * self.constraint_width(c).max(1)
            }
            _ => w,
        };
        let dyn_w = match (tmark, constraint) {
            (Some(tm), Some(c))
                if self.constraint_shapes_width(tm)
                    && !self.text(c).contains("<>")
                    && !self.constraint_folds(c) =>
            {
                Some(self.dyn_width_expr(tm, c))
            }
            _ => None,
        };
        (w, signed, real, dyn_w)
    }

    /// Lower a subprogram local's per-call initializer (`variable v :
    /// integer := 0;` — subprogram variables are re-initialized per call,
    /// so it is prepended to the body like a dynamic local constant). A
    /// string target's `(others => ' ')` replicate must cover the whole
    /// object (see `agg_target_w`); a dynamic-width local re-derives the
    /// replicate count from the dynamic width expression (the generic
    /// lowering baked the fallback width).
    fn lower_fn_local_init(
        &mut self,
        dk: &[SyntaxNode],
        colon: usize,
        tmark: Option<&SyntaxNode>,
        w: usize,
        dyn_w: &Option<Expr>,
    ) -> Option<Expr> {
        let init_node = dk[colon + 1..]
            .iter()
            .position(|k| k.kind() == SyntaxKind::Punct && self.text(k) == "=")
            .and_then(|e| {
                dk[colon + 1 + e + 1..]
                    .iter()
                    .find(|k| is_expr_node(k.kind()))
                    .cloned()
            });
        let mut init_ex = init_node.as_ref().map(|node| {
            let saved_w = self.agg_target_w;
            if node.kind() == SyntaxKind::Aggregate && tmark.is_some_and(|tm| self.string_typed(tm))
            {
                self.agg_target_w = Some(w);
            }
            let ex = self.lower_expr(node, &self.consts.clone());
            self.agg_target_w = saved_w;
            ex
        });
        if let (Some(wexpr), Some(node)) = (dyn_w, &init_node) {
            if let Some(ex) = self.dyn_others_init(node, wexpr) {
                init_ex = Some(ex);
            }
        }
        init_ex
    }

    /// Lower one `variable` declarative item of a subprogram: slot its
    /// names, resolve the width (with static array-constraint expansion and
    /// dynamic-constraint fallback), register shapes/dyn mappings, and queue
    /// the per-call initializer. Subprogram locals are flat values, never
    /// mems. Malformed decls (no `:`) are skipped.
    fn lower_fn_local_variable(
        &mut self,
        d: &SyntaxNode,
        slots: &mut Vec<(String, usize)>,
        locals: &mut Vec<SignalIr>,
        local_types: &mut Vec<(String, String)>,
        dyn_locals: &mut Vec<(SigId, Expr)>,
        pending_const_inits: &mut Vec<(SigId, Expr)>,
    ) {
        let dk = real_children(d);
        let Some(colon) = dk
            .iter()
            .position(|k| k.kind() == SyntaxKind::Punct && self.text(k) == ":")
        else {
            return;
        };
        let names: Vec<String> = dk[..colon]
            .iter()
            .filter(|k| k.kind() == SyntaxKind::Name)
            .map(|k| self.text(k))
            .collect();
        let tmark = dk[colon + 1..]
            .iter()
            .find(|k| k.kind() == SyntaxKind::Name);
        let constraint = dk[colon + 1..]
            .iter()
            .find(|k| k.kind() == SyntaxKind::Range);
        let (w, signed, real, dyn_w) = self.resolve_fn_local_width(tmark, constraint, d.span());
        let init_ex = self.lower_fn_local_init(&dk, colon, tmark, w, &dyn_w);
        for n in names {
            let id = slots.len() as SigId;
            if let Some(tm) = &tmark {
                local_types.push((n.clone(), self.kw(tm)));
            }
            slots.push((n.clone(), w));
            self.names.insert(n.clone(), id);
            self.sig_signed.insert(id, signed);
            self.sig_width.insert(id, w);
            self.record_obj_shape(id, tmark, constraint);
            if let Some(wexpr) = &dyn_w {
                dyn_locals.push((id, wexpr.clone()));
                // The addressing remaps must size slices from the
                // per-call width, not the registration fallback.
                self.sig_dynw.insert(id, wexpr.clone());
            }
            if let Some(ex) = &init_ex {
                pending_const_inits.push((id, ex.clone()));
            }
            locals.push(SignalIr {
                name: n,
                width: w,
                kind: if real { SigKind::Real } else { SigKind::Wire },
                init: None,
                mem: None,
                signed,
            });
        }
    }

    /// Lower a subprogram's declarative part: pre-register nested
    /// subprogram stubs, walk the declarative items in textual order (each
    /// may reference the ones before it — parameters, types, constants,
    /// locals), map record-typed locals, then lower the nested subprogram
    /// bodies over their stubs.
    fn lower_fn_declarative_items(
        &mut self,
        decl_nodes: &[SyntaxNode],
        nested_fns: &[SyntaxNode],
        fd: &mut FnLocalDecls,
    ) {
        for k in nested_fns {
            if let Some(stub) = self.subprogram_stub(k) {
                self.register_func(stub);
            }
        }
        for d in decl_nodes {
            match d.kind() {
                SyntaxKind::PortDecl => {} // parameters were slotted above
                SyntaxKind::TypeDecl => self.lower_type_decl(d),
                SyntaxKind::SubtypeDecl => self.lower_subtype_decl(d),
                SyntaxKind::ViewDecl => self.lower_view_decl(d),
                SyntaxKind::ConstantDecl => self.lower_fn_local_constant(
                    d,
                    &mut fd.slots,
                    &mut fd.locals,
                    &mut fd.local_types,
                    &mut fd.pending_const_inits,
                ),
                SyntaxKind::AliasDecl => self.lower_alias_decl(d),
                SyntaxKind::AttrDecl => {
                    self.register_attr_decl(d);
                }
                SyntaxKind::FileDecl => self.lower_file_decl(d),
                SyntaxKind::VariableDecl => self.lower_fn_local_variable(
                    d,
                    &mut fd.slots,
                    &mut fd.locals,
                    &mut fd.local_types,
                    &mut fd.dyn_locals,
                    &mut fd.pending_const_inits,
                ),
                _ => {}
            }
        }
        // Record-typed locals (declared in the loop above): same treatment.
        self.map_slot_records(&fd.slots, &fd.local_types);
        for k in nested_fns {
            if let Some(fi) = self.lower_function_node(k) {
                self.register_func(fi);
            }
        }
    }

    /// Lower a subprogram's statement part: loop variables allocated while
    /// lowering get slots after the declared ones (appended to `locals` so
    /// the runtime and const-eval slot space match), and pending
    /// constant-initializer assignments are prepended to the body (they run
    /// on every call, before any statement).
    fn lower_fn_body(
        &mut self,
        stmt_nodes: &[SyntaxNode],
        slots: &[(String, usize)],
        locals: &mut Vec<SignalIr>,
        pending_const_inits: Vec<(SigId, Expr)>,
    ) -> Stmt {
        self.extra_base = slots.len();
        self.extra_signals.clear();
        let body = if stmt_nodes.len() == 1 {
            self.lower_seq_stmt(&stmt_nodes[0])
        } else {
            Stmt::Block(stmt_nodes.iter().map(|k| self.lower_seq_stmt(k)).collect())
        };
        let body = if pending_const_inits.is_empty() {
            body
        } else {
            let mut ss: Vec<Stmt> = pending_const_inits
                .into_iter()
                .map(|(id, e)| Stmt::Assign {
                    lhs: Expr::Sig(id),
                    rhs: e,
                    nba: false,
                })
                .collect();
            ss.push(body);
            Stmt::Block(ss)
        };
        for sig in self.extra_signals.drain(..) {
            locals.push(sig);
        }
        self.extra_base = 0;
        body
    }

    /// Width for an unconstrained vector/string return type mark (`return
    /// bit_vector` / `return string`): the widest returned expression. The
    /// declared width (fallback 32) seeds the max.
    fn unconstrained_ret_width(
        &self,
        ret_mark: &Option<SyntaxNode>,
        body: &Stmt,
        ret_width: usize,
    ) -> usize {
        let is_uncon = ret_mark
            .as_ref()
            .map(|tm| self.kw(tm))
            .and_then(|tn| self.types.get(&tn))
            .is_some_and(|ti| matches!(ti, TypeInfo::Vector { .. }));
        if !is_uncon {
            return ret_width;
        }
        let mut rets = Vec::new();
        collect_return_exprs(body, &mut rets);
        let mut w = ret_width;
        for r in &rets {
            w = w.max(self.expr_width_est(r));
        }
        w
    }

    pub(super) fn lower_function_node(&mut self, f: &SyntaxNode) -> Option<FuncIr> {
        let hdr = self.split_subprogram_header(f)?;
        let SubprogramHeader {
            name,
            ret_mark,
            decl_nodes,
            nested_fns,
            stmt_nodes,
        } = hdr;
        let FormalParams {
            args,
            arg_types,
            arg_shapes,
            defaults,
            ret_width,
            ..
        } = self.lower_formal_params(&decl_nodes, &ret_mark, f);
        // Lower the body in slot space (slot 0 = return value; the install
        // also pushes the enclosing `names` map for the scope guard below).
        let (slots, fn_slots) =
            self.install_fn_slots(&name, ret_width, &args, &arg_types, &arg_shapes);
        // Formals/locals now in `names`: outer constants of the same name in
        // the lowering env must not shadow them (see `fn_scope`).
        self.fn_scope += 1;
        // Function-local constants whose initializer is not const-evalable at
        // lower time (e.g. `constant msb := min(arg'length, size) - 1;` in
        // std_logic_arith): they get a runtime slot and the initializer is
        // evaluated on every call, prepended to the body as an assignment.
        // Locals whose constraint depends on subprogram formals — const-eval
        // re-folds their widths per call (see `FuncIr.dyn_locals`).
        let mut fd = FnLocalDecls {
            slots,
            locals: Vec::new(),
            local_types: Vec::new(),
            pending_const_inits: Vec::new(),
            dyn_locals: Vec::new(),
        };
        // Pre-register nested subprograms so constant initializers in the
        // declarative part can reference them (`constant r : string := f;`);
        // real bodies are lowered after the loop and overwrite the stubs.
        // Declarative items in textual order: each may reference the ones
        // before it (parameters, types, constants, locals). Nested function
        // declarations lower last, now that all outer slots are visible.
        self.lower_fn_declarative_items(&decl_nodes, &nested_fns, &mut fd);
        let body = self.lower_fn_body(
            &stmt_nodes,
            &fd.slots,
            &mut fd.locals,
            fd.pending_const_inits,
        );
        self.names = self.names_stack.pop().unwrap_or_default();
        self.fn_scope -= 1;
        let ret_width = self.unconstrained_ret_width(&ret_mark, &body, ret_width);
        // Reset the slot-id record mappings to their entry state (see
        // `FnSlotScope`): this runs after the return-width fixup above,
        // which still reads the function's own slot mappings.
        fn_slots.restore(self);
        Some(FuncIr {
            name,
            ret_width: ret_width.max(1),
            args,
            defaults,
            locals: fd.locals,
            dyn_locals: fd.dyn_locals,
            body,
        })
    }
}

/// A subprogram's interface, collected from its PortDecl declarative items.
struct FormalParams {
    /// name → lowered width, so a `b : a'subtype` formal adopts its base
    param_w: HashMap<String, usize>,
    /// (name, width, declared type name) — the dispatch key set
    args: Vec<(String, usize, String)>,
    /// (name, type mark) for record/elem-record/protected mapping
    arg_types: Vec<(String, String)>,
    /// (name, type mark, constraint) for `record_obj_shape`
    arg_shapes: Vec<(String, Option<SyntaxNode>, Option<SyntaxNode>)>,
    /// per-formal interface default, const-evaled
    defaults: Vec<Option<Value>>,
    /// return width, resolved after the formal loop
    ret_width: usize,
}

/// One subprogram's syntactic parts, split from the CST: the bare name, the
/// return type mark, declarative items in textual order, nested function
/// declarations, and the statement nodes after `begin`.
struct SubprogramHeader {
    name: String,
    ret_mark: Option<SyntaxNode>,
    decl_nodes: Vec<SyntaxNode>,
    nested_fns: Vec<SyntaxNode>,
    stmt_nodes: Vec<SyntaxNode>,
}
