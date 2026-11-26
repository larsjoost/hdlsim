//! Subprogram lowering: function/procedure declarations and stubs, dynamic-width local machinery (dyn_width_expr/dyn_others_init), and the lower_function_node body lowerer.
//!
//! Extracted verbatim from vhdl_lower.rs (pure move, no logic changes).

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
                SyntaxKind::PortDecl
                | SyntaxKind::VariableDecl
                | SyntaxKind::ConstantDecl
                | SyntaxKind::AliasDecl
                | SyntaxKind::AttrDecl
                | SyntaxKind::TypeDecl
                | SyntaxKind::SubtypeDecl
                | SyntaxKind::FileDecl
                    if !seen_begin =>
                {
                    decl_nodes.push(k.clone())
                }
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
        if let (Some(rm), Some(om)) = (&ret_mark, &of_mark) {
            let rn = self.kw(rm).to_ascii_lowercase();
            let on = self.kw(om).to_ascii_lowercase();
            let ti = self
                .types
                .get(&on)
                .cloned()
                .unwrap_or(TypeInfo::Vector { signed: false });
            self.types.insert(rn, ti);
        }
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
    fn map_slot_records(&mut self, slots: &[(String, usize)], types: &[(String, String)]) {
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
        let (w, signed, _, real) = match tmark {
            Some(tm) => self.resolve_type(tm, constraint, d.span()),
            None => (32, false, None, false),
        };
        // An unconstrained array mark with a static constraint
        // (`register_definition_array_t(0 to 63)`): resolve_type
        // returns the bare element width, but the local's flat
        // slot covers every element the constraint adds.
        // Subprogram locals are flat values, never mems. A
        // dynamic constraint keeps the fallback width and the
        // recorded `dyn_w` below.
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
        // A constraint that references subprogram formals
        // (`variable s : string(1 to size)`) can't fold at
        // registration — formals have no values yet — so the
        // recorded width is a fallback. Record a bit-width
        // expression for const-eval to fold per call, and size
        // an OTHERS initializer from it too. Only array-like
        // types qualify: a record's flat width is the sum of
        // its fields, and a record-element constraint
        // (`t_node(name(1 to n))`) selects field ranges —
        // lowering its element name as an identifier would
        // misfire E0401.
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
        // `variable v : integer := 0;` — the initializer runs on
        // every call (VHDL subprogram variables are re-initialized
        // per call), so lower it and prepend `v := init;` to the
        // body like a dynamic local constant.
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
            // A string target's `(others => ' ')` replicate must
            // cover the whole object (see `agg_target_w`); a
            // dynamic-width local re-derives the count below.
            let saved_w = self.agg_target_w;
            if node.kind() == SyntaxKind::Aggregate && tmark.is_some_and(|tm| self.string_typed(tm))
            {
                self.agg_target_w = Some(w);
            }
            let ex = self.lower_expr(node, &self.consts.clone());
            self.agg_target_w = saved_w;
            ex
        });
        // A dynamic-width local with an OTHERS initializer must
        // self-size: re-emit the replicate with the count derived
        // from the dynamic width expression (the generic lowering
        // baked the fallback width).
        if let (Some(wexpr), Some(node)) = (&dyn_w, &init_node) {
            if let Some(ex) = self.dyn_others_init(node, wexpr) {
                init_ex = Some(ex);
            }
        }
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
            mut ret_width,
            ..
        } = self.lower_formal_params(&decl_nodes, &ret_mark, f);
        // Lower the body in slot space (slot 0 = return value). Install the
        // return + parameter slots first so function-local constants can
        // reference parameters (`constant pos : time := ifelse(T >= 0 ns, T, -T);`).
        let saved_names = std::mem::take(&mut self.names);
        self.names_stack.push(saved_names);
        let mut slots: Vec<(String, usize)> = vec![(name.clone(), ret_width.max(1))];
        slots.extend(args.iter().map(|(n, w, _)| (n.clone(), *w)));
        self.names = slots
            .iter()
            .enumerate()
            .map(|(i, (n, _))| (n.clone(), i as SigId))
            .collect();
        // Formals are sized by the call's actuals: mark their slot ids so
        // `'length`/`'high` on them lower symbolically even when an earlier
        // subprogram left a stale `sig_width`/`sig_dims` entry under the same
        // aliased slot id. Saved/restored for nested subprograms below.
        let saved_formals = std::mem::take(&mut self.fn_formals);
        self.fn_formals = (1..slots.len()).map(|i| i as SigId).collect();
        // Slot ids alias across subprograms: a previously lowered
        // subprogram's shape entries (`sig_asc`, `sig_elem`, `sig_dynw`,
        // `sig_dims`) and record mappings under the same id would
        // mis-address this one's slices and fold its attributes from
        // another function's object. Capture the entry state, drop it for
        // the slots about to be registered, and restore at exit;
        // `record_obj_shape` re-adds what this subprogram's formals have.
        let saved_shapes = SlotShapeSave::capture(self, slots.len());
        saved_shapes.drop_entries(self);
        for (n, tmark, constraint) in &arg_shapes {
            if let Some(&id) = self.names.get(n) {
                self.record_obj_shape(id, tmark.as_ref(), constraint.as_ref());
            }
        }
        // Formals/locals now in `names`: outer constants of the same name in
        // the lowering env must not shadow them (see `fn_scope`).
        self.fn_scope += 1;
        // Record-typed parameters: remember the record type so `arg.field`
        // resolves even in declarative-item initializers (constants), which
        // are lowered before the loop below. Slot ids are per-function, so
        // entries left by a previously lowered function must be dropped
        // first — a formal whose type is not a record/array-of-records
        // would otherwise inherit the stale record of an earlier function's
        // same-numbered slot. The entry-state capture and exit reset are
        // part of `SlotShapeSave` above.
        self.map_slot_records(&slots, &arg_types);
        // Pre-register nested subprograms so constant initializers in the
        // declarative part can reference them (`constant r : string := f;`);
        // real bodies are lowered after the loop and overwrite the stubs.
        for k in &nested_fns {
            if let Some(stub) = self.subprogram_stub(k) {
                self.register_func(stub);
            }
        }
        // Declarative items in textual order: each may reference the ones
        // before it (parameters, types, constants, locals).
        let mut locals: Vec<SignalIr> = Vec::new();
        let mut local_types: Vec<(String, String)> = Vec::new();
        // Function-local constants whose initializer is not const-evalable at
        // lower time (e.g. `constant msb := min(arg'length, size) - 1;` in
        // std_logic_arith): they get a runtime slot and the initializer is
        // evaluated on every call, prepended to the body as an assignment.
        let mut pending_const_inits: Vec<(SigId, Expr)> = Vec::new();
        // Locals whose constraint depends on subprogram formals — const-eval
        // re-folds their widths per call (see `FuncIr.dyn_locals`).
        let mut dyn_locals: Vec<(SigId, Expr)> = Vec::new();
        for d in &decl_nodes {
            match d.kind() {
                SyntaxKind::PortDecl => {} // parameters were slotted above
                SyntaxKind::TypeDecl => self.lower_type_decl(d),
                SyntaxKind::SubtypeDecl => self.lower_subtype_decl(d),
                SyntaxKind::ViewDecl => self.lower_view_decl(d),
                SyntaxKind::ConstantDecl => self.lower_fn_local_constant(
                    d,
                    &mut slots,
                    &mut locals,
                    &mut local_types,
                    &mut pending_const_inits,
                ),
                SyntaxKind::AliasDecl => self.lower_alias_decl(d),
                SyntaxKind::AttrDecl => {
                    self.register_attr_decl(d);
                }
                SyntaxKind::FileDecl => self.lower_file_decl(d),
                SyntaxKind::VariableDecl => self.lower_fn_local_variable(
                    d,
                    &mut slots,
                    &mut locals,
                    &mut local_types,
                    &mut dyn_locals,
                    &mut pending_const_inits,
                ),
                _ => {}
            }
        }
        // Record-typed locals (declared in the loop above): same treatment.
        self.map_slot_records(&slots, &local_types);
        // Nested function declarations, now that all outer slots are visible.
        for k in &nested_fns {
            if let Some(fi) = self.lower_function_node(k) {
                self.register_func(fi);
            }
        }
        // Loop variables allocated while lowering the body get slots after the
        // declared ones (they are appended to `locals` below so the runtime and
        // const-eval slot space matches).
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
        self.names = self.names_stack.pop().unwrap_or_default();
        self.fn_scope -= 1;
        // Unconstrained vector/string return type (`return bit_vector` /
        // `return string`): take the width of the returned expressions.
        let ret_is_uncon_vector = ret_mark
            .as_ref()
            .map(|tm| self.kw(tm))
            .and_then(|tn| self.types.get(&tn))
            .is_some_and(|ti| matches!(ti, TypeInfo::Vector { .. }));
        if ret_is_uncon_vector {
            let mut rets = Vec::new();
            collect_return_exprs(&body, &mut rets);
            for r in &rets {
                ret_width = ret_width.max(self.expr_width_est(r));
            }
        }
        // Reset the slot-id record mappings to their entry state: entries
        // the body inserted for record-typed formals must not leak into the
        // enclosing scope (slot ids alias module signal ids), and module
        // entries the body must not touch keep their pre-call values. This
        // runs after the return-width fixups above, which still read the
        // function's own slot mappings.
        saved_shapes.restore(self);
        self.fn_formals = saved_formals;
        Some(FuncIr {
            name,
            ret_width: ret_width.max(1),
            args,
            defaults,
            locals,
            dyn_locals,
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

/// Entry-state snapshot of the per-slot shape and record maps for the slot
/// ids a subprogram is about to register. Slot ids alias the flat-signal id
/// space — module signals can hold the same ids — so a subprogram's entries
/// must not leak into the enclosing scope after its body lowers: captured at
/// entry, dropped for the new slots, restored exactly at exit. The record
/// maps (`rec`/`elem_rec`/`prot`) are captured here too: nothing between the
/// capture point and their original later capture site mutates them, so one
/// snapshot is behavior-identical to the previous split capture.
struct SlotShapeSave {
    len: usize,
    elem: HashMap<SigId, Option<usize>>,
    asc: HashMap<SigId, Option<(i64, bool)>>,
    dynw: HashMap<SigId, Option<Expr>>,
    dims: HashMap<SigId, Option<Vec<(i64, usize)>>>,
    rec: HashMap<SigId, Option<String>>,
    elem_rec: HashMap<SigId, Option<String>>,
    prot: HashMap<SigId, Option<String>>,
}

fn capture_map<T: Clone>(map: &HashMap<SigId, T>, len: usize) -> HashMap<SigId, Option<T>> {
    (0..len)
        .map(|i| {
            let id = i as SigId;
            (id, map.get(&id).cloned())
        })
        .collect()
}

fn restore_map<T: Clone>(map: &mut HashMap<SigId, T>, saved: HashMap<SigId, Option<T>>) {
    for (id, v) in saved {
        match v {
            Some(v) => {
                map.insert(id, v);
            }
            None => {
                map.remove(&id);
            }
        }
    }
}

impl SlotShapeSave {
    fn capture(lx: &Lower<'_>, len: usize) -> Self {
        Self {
            len,
            elem: capture_map(&lx.sig_elem, len),
            asc: capture_map(&lx.sig_asc, len),
            dynw: capture_map(&lx.sig_dynw, len),
            dims: capture_map(&lx.sig_dims, len),
            rec: capture_map(&lx.sig_rec, len),
            elem_rec: capture_map(&lx.sig_elem_rec, len),
            prot: capture_map(&lx.sig_prot, len),
        }
    }

    /// Drop the shape entries under the subprogram's slot ids so a stale
    /// entry from a previously lowered subprogram cannot mis-address this
    /// one's slices or fold its attributes from another function's object.
    /// `record_obj_shape` re-adds what this subprogram's objects have.
    fn drop_entries(&self, lx: &mut Lower<'_>) {
        for i in 0..self.len {
            let id = i as SigId;
            lx.sig_asc.remove(&id);
            lx.sig_elem.remove(&id);
            lx.sig_dynw.remove(&id);
            lx.sig_dims.remove(&id);
        }
    }

    fn restore(self, lx: &mut Lower<'_>) {
        restore_map(&mut lx.sig_elem, self.elem);
        restore_map(&mut lx.sig_asc, self.asc);
        restore_map(&mut lx.sig_dynw, self.dynw);
        restore_map(&mut lx.sig_dims, self.dims);
        restore_map(&mut lx.sig_rec, self.rec);
        restore_map(&mut lx.sig_elem_rec, self.elem_rec);
        restore_map(&mut lx.sig_prot, self.prot);
    }
}
