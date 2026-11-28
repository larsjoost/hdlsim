//! Object-declaration lowering: aliases, signals, files, constants (module- and function-local), and protected-method stubs.
//!
//! Extracted verbatim from vhdl_lower.rs (pure move, no logic changes).

use super::*;

impl<'a> Lower<'a> {
    /// `alias name is target;` — a whole-signal target is bound in `names`
    /// (so sensitivity lists etc. see it), a constant target in `consts`,
    /// anything else (slice/index select) in the expression-alias table.
    pub(super) fn lower_alias_decl(&mut self, d: &SyntaxNode) {
        let kids = real_children(d);
        let Some(name_pos) = kids.iter().position(|k| k.kind() == SyntaxKind::Name) else {
            return;
        };
        let name = self.text(&kids[name_pos]).to_ascii_lowercase();
        let Some(is_pos) = kids
            .iter()
            .position(|k| k.kind() == SyntaxKind::Ident && self.kw(k) == "is")
        else {
            return;
        };
        let Some(target) = kids[is_pos + 1..]
            .iter()
            .find(|k| is_expr_node(k.kind()) || k.kind() == SyntaxKind::ExtName)
        else {
            return;
        };
        // External-name alias: `alias a is << signal ^.dut.s : t >>;` (also
        // with an absolute `.dut.s` path). The hierarchical target is not
        // simulated — register a placeholder so uses of the alias resolve.
        if target.kind() == SyntaxKind::ExtName {
            self.aliases.insert(name.clone(), Expr::Lit(Value::x(1)));
            return;
        }
        // Type alias: `alias ptr_t is integer_vector_ptr_t;` registers the
        // alias name as a type.
        let tname = self.kw(target);
        if let Some(ti) = self.types.get(&tname).cloned() {
            // Type alias (incl. a view name): `alias V2 is V1;` — done.
            self.types.insert(name.clone(), ti);
            return;
        }
        // Subprogram alias with a selected target:
        // `alias RISING_EDGE is STD.STANDARD.RISING_EDGE [BIT return BOOLEAN];`
        // — bind to the package's function so calls to the alias resolve.
        if matches!(target.kind(), SyntaxKind::Name) {
            let parts: Vec<&str> = tname.split('.').collect();
            if parts.len() >= 3 {
                let unit = parts[parts.len() - 2];
                let item = parts[parts.len() - 1];
                if let Some(p) = self.pkgs.get(unit) {
                    if p.functions
                        .iter()
                        .any(|f| f.name.to_ascii_lowercase() == item)
                    {
                        self.aliases.insert(
                            name.clone(),
                            Expr::Call {
                                name: item.to_string(),
                                args: Vec::new(),
                                shapes: Vec::new(),
                            },
                        );
                        return;
                    }
                }
                // Unresolvable selected subprogram alias: silent placeholder.
                self.aliases.insert(name.clone(), Expr::Lit(Value::x(1)));
                return;
            }
        }
        // Record alias: `alias LAW : SomeRec'subtype is sig.field;` — remember
        // the record type so `LAW.field` selects slice the aliased expression.
        let colon = kids
            .iter()
            .position(|k| k.kind() == SyntaxKind::Punct && self.text(k) == ":");
        if let Some(c) = colon {
            if c > name_pos && c < is_pos {
                if let Some(tm) = kids[c + 1..is_pos]
                    .iter()
                    .find(|k| k.kind() == SyntaxKind::Name)
                {
                    let tn = self.text(tm).to_ascii_lowercase();
                    if let Some(rn) = self.as_record(&tn) {
                        self.alias_rec.insert(name.clone(), rn);
                    }
                }
            }
        }
        let e = self.lower_expr(target, &self.consts.clone());
        match &e {
            Expr::Sig(id) => {
                self.names.insert(name.clone(), *id);
            }
            Expr::Lit(v) => {
                Rc::make_mut(&mut self.consts).insert(name.clone(), v.clone());
            }
            _ => {}
        }
        // No declared subtype: derive the record type from a dotted target
        // (`alias LAW is AxiDefaults.WriteAddress;`).
        if !self.alias_rec.contains_key(&name) && matches!(target.kind(), SyntaxKind::Name) {
            let ttext = self.text(target);
            let segs: Vec<&str> = ttext.split('.').collect();
            if segs.len() >= 2 {
                let base = segs[0].to_ascii_lowercase();
                let path: Vec<String> = segs[1..].iter().map(|s| s.to_ascii_lowercase()).collect();
                let path_refs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
                if let Some(tn) = self.record_type_at_path(&base, &path_refs) {
                    if matches!(self.types.get(&tn), Some(TypeInfo::Record { .. })) {
                        self.alias_rec.insert(name.clone(), tn);
                    }
                }
            }
        }
        self.aliases.insert(name, e);
    }

    pub(super) fn lower_signal_decl(&mut self, module: &mut ModuleIr, d: &SyntaxNode) {
        let kids = real_children(d);
        let colon = kids
            .iter()
            .position(|k| k.kind() == SyntaxKind::Punct && self.text(k) == ":");
        let Some(colon) = colon else {
            self.err(d.span(), "malformed signal declaration");
            return;
        };
        let names: Vec<String> = kids[..colon]
            .iter()
            .filter(|k| k.kind() == SyntaxKind::Name)
            .map(|k| self.text(k))
            .collect();
        let type_mark = kids[colon + 1..]
            .iter()
            .find(|k| k.kind() == SyntaxKind::Name);
        let constraint = kids[colon + 1..]
            .iter()
            .find(|k| k.kind() == SyntaxKind::Range);
        // The initializer follows `:=`; it may be a bare name (a constant or
        // a parameterless function call — `SIGNAL w : natural := C_WIDTH;`),
        // so look past the `=` rather than skipping Name nodes (same pattern
        // as `lower_constant_decl`).
        let init = kids[colon + 1..]
            .iter()
            .position(|k| k.kind() == SyntaxKind::Punct && self.text(k) == "=")
            .and_then(|e| {
                kids[colon + 1 + e + 1..]
                    .iter()
                    .find(|k| is_expr_node(k.kind()))
                    .cloned()
            });
        let span = d.span();
        let type_name = type_mark.map(|tm| self.text(tm).to_ascii_lowercase());
        let (width, signed, mem, real) = match type_mark {
            Some(tm) => self.resolve_type(tm, constraint, span),
            None => (1, false, None, false),
        };
        let width = self.mem_row_width(type_mark, width);
        let kind = if real { SigKind::Real } else { SigKind::Wire };
        let init_v = init.and_then(|e| {
            // A string target's `(others => ' ')` replicate must cover the
            // whole object; without the target context it baked a fixed
            // 32-bit count (see `agg_target_w`).
            let saved_w = self.agg_target_w;
            if e.kind() == SyntaxKind::Aggregate
                && type_mark.is_some_and(|tm| self.string_typed(tm))
            {
                self.agg_target_w = Some(width);
            }
            let ex = self.lower_expr(&e, &self.consts.clone());
            self.agg_target_w = saved_w;
            self.ce(&ex)
        });
        for n in &names {
            // Duplicate declaration in the SAME scope: keep the first
            // registration. A declaration in an INNER scope (block, process)
            // must shadow, not collide — the name is registered in the fresh
            // scope map (see `lower_scoped`), so only `self.names` counts
            // here; the enclosing scopes on `names_stack` are irrelevant.
            let dup =
                self.names.contains_key(n) || self.names.keys().any(|k| k.eq_ignore_ascii_case(n));
            if dup {
                continue;
            }
            let id = module.signals.len() as SigId;
            module.signals.push(SignalIr {
                name: n.clone(),
                width,
                kind,
                init: init_v.clone(),
                mem,
                signed,
            });
            if let Some(d) = mem {
                self.mems.insert(id, d);
            }
            self.sig_signed.insert(id, signed);
            self.sig_width.insert(id, width);
            self.record_obj_shape(id, type_mark, constraint);
            if let Some(tn) = &type_name {
                if let Some(rn) = self.as_record(tn) {
                    self.sig_rec.insert(id, rn.clone());
                    module
                        .real_ranges
                        .insert(n.clone(), self.record_real_ranges(&rn));
                }
                // Array: remember the element record type for
                // `arr(i).field` selects (arrays of records), and the
                // dims from the signal's own constraint if present, else
                // from the array type declaration.
                if let Some(en) = self.as_elem_record(tn) {
                    self.sig_elem_rec.insert(id, en);
                }
                if let Some(TypeInfo::Array { dims, .. }) = self.types.get(tn) {
                    if !dims.is_empty() {
                        let d = match constraint {
                            Some(c) => self.constraint_dims(c),
                            None => dims.clone(),
                        };
                        self.sig_dims.insert(id, d);
                    } else if let Some(td) = self.type_dims.get(tn).cloned() {
                        self.sig_dims.insert(id, td);
                    } else if let Some(c) = &constraint {
                        // Unconstrained array base with an inline object
                        // constraint (`signal m : matrix_t(0 to 2, 0 to 4)`):
                        // the dims belong to the object, not the type —
                        // `'length(d)`-style attributes and `m(i, j)` flat
                        // indexes must see them. 1-D objects keep the
                        // existing mems/width handling.
                        if !self.text(c).contains("<>") {
                            let cd = self.constraint_dims(c);
                            if !cd.is_empty() {
                                self.set_sig_dims_from_constraint(id, cd, n);
                            }
                        }
                    }
                } else if let Some(td) = self.type_dims.get(tn).cloned() {
                    // Vector subtype carrying its own range.
                    self.sig_dims.insert(id, td);
                }
                // Protected-type object: the signal holds a 64-bit handle.
                // Remember the type for member selects even when the type
                // comes from a use'd package (no local body); allocate +
                // initialize only for locally declared types (ctor exists).
                if self.prot_types.contains_key(tn)
                    || matches!(self.types.get(tn), Some(TypeInfo::Handle))
                {
                    self.sig_prot.insert(id, tn.clone());
                }
                if self.prot_types.contains_key(tn) {
                    module.processes.push(ProcessIr {
                        kind: ProcKind::Initial,
                        sens: Sens::List(Vec::new()),
                        body: Stmt::SysCall {
                            name: format!("$new::{}", tn),
                            args: vec![Expr::Sig(id)],
                            shapes: Vec::new(),
                        },
                        origin: format!("init protected object {n}"),
                        gen_scope: self.gen_path.join("."),
                        initial: false,
                    });
                }
            }
            self.names.insert(n.clone(), id);
        }
    }

    /// `file f [, g] : text [open <mode> is "path"];` — bind each file name to
    /// its path as a string constant (`input`/`output` default to the
    /// stdin/stdout markers), so textio syscalls can find the file at runtime.
    pub(super) fn lower_file_decl(&mut self, d: &SyntaxNode) {
        let kids = real_children(d);
        let Some(colon) = kids
            .iter()
            .position(|k| k.kind() == SyntaxKind::Punct && self.text(k) == ":")
        else {
            return;
        };
        let names: Vec<String> = kids[..colon]
            .iter()
            .filter(|k| k.kind() == SyntaxKind::Name)
            .map(|k| self.kw(k))
            .collect();
        let path = kids[colon + 1..]
            .iter()
            .find(|k| k.kind() == SyntaxKind::Literal)
            .map(|k| self.text(k).trim_matches('"').to_string());
        for n in names {
            let p = match &path {
                Some(p) => p.clone(),
                None if n == "input" => "stdin".to_string(),
                None if n == "output" => "stdout".to_string(),
                // Path-less file: a unique marker constant, resolved at
                // runtime — `$textio_file_open` binds it to the actual path
                // (kernel-side map), writeline/readline consult the map.
                None => {
                    let marker = format!("$file:{}#{}", n, self.file_seq);
                    self.file_seq += 1;
                    marker
                }
            };
            Rc::make_mut(&mut self.consts).insert(n, string_value(&p));
        }
    }

    pub(super) fn lower_constant_decl(&mut self, d: &SyntaxNode) {
        let kids = real_children(d);
        let colon = kids
            .iter()
            .position(|k| k.kind() == SyntaxKind::Punct && self.text(k) == ":");
        let Some(colon) = colon else { return };
        let names: Vec<String> = kids[..colon]
            .iter()
            .filter(|k| k.kind() == SyntaxKind::Name)
            .map(|k| self.text(k))
            .collect();
        let type_mark = kids[colon + 1..]
            .iter()
            .find(|k| k.kind() == SyntaxKind::Name);
        // The initializer follows `:=`; it may be a bare name (e.g. a generic
        // reference `constant c_w : natural := WIDTH;`), so look past the
        // `=` rather than skipping Name nodes.
        let eq = kids[colon + 1..]
            .iter()
            .position(|k| k.kind() == SyntaxKind::Punct && self.text(k) == "=");
        let init = match eq {
            Some(e) => kids[colon + 1 + e + 1..]
                .iter()
                .find(|k| is_expr_node(k.kind()))
                .cloned(),
            None => kids[colon + 1..]
                .iter()
                .find(|k| is_expr_node(k.kind()) && k.kind() != SyntaxKind::Name)
                .cloned(),
        };
        let env = self.consts.clone();
        let width = type_mark
            .map(|tm| self.resolve_type(tm, None, d.span()).0)
            .unwrap_or(32);
        // No initializer (`constant c : t;` — value assigned in the package
        // body): bind the name to X so uses resolve.
        let Some(init) = init else {
            for n in &names {
                Rc::make_mut(&mut self.consts)
                    .insert(n.to_ascii_lowercase(), Value::x(width.max(1)));
            }
            return;
        };
        // Record-typed constant: order named aggregate fields by the record.
        // Array-typed constant: place named-index/others choices over the
        // constraint range. Both recurse into nested aggregate-typed fields.
        let constraint = kids[colon + 1..]
            .iter()
            .take_while(|k| !(k.kind() == SyntaxKind::Punct && self.text(k) == "="))
            .find(|k| k.kind() == SyntaxKind::Range)
            .cloned();
        // Inline array constraint (`constant m : matrix_t(0 to 2, 0 to 4) :=
        // …`): register the object's shape so `'length(d)`-style attributes
        // on the constant fold and index/slice addressing remaps (the
        // constraint is not part of the type mark).
        self.record_const_shapes(type_mark, constraint.as_ref(), &names);
        let want_str = type_mark.is_some_and(|tm| self.string_typed(tm));
        let saved_str = self.str_ctx;
        self.str_ctx = want_str;
        let ex = match type_mark {
            Some(tm) if init.kind() == SyntaxKind::Aggregate => {
                let tn = self.text(tm).to_ascii_lowercase();
                match self.types.get(&tn).cloned() {
                    Some(TypeInfo::Record { .. }) => {
                        let fields = self.record_fields_of_type(&tn);
                        if fields.is_empty() {
                            self.lower_expr(&init, &env)
                        } else {
                            self.lower_typed_record_aggregate(&init, &tn, &fields, &env)
                        }
                    }
                    Some(TypeInfo::Array { elem, .. }) => {
                        let elem = elem.max(1);
                        let (n, lo) = match &constraint {
                            Some(c) => {
                                let n = self.constraint_width(c);
                                let lo = self.range_low(c, &env).unwrap_or(0);
                                (Some(n), lo)
                            }
                            None => {
                                let w = self.resolve_type(tm, None, d.span()).0;
                                self.array_agg_len(w, elem)
                            }
                        };
                        match n {
                            Some(n) if n > 0 => self
                                .lower_array_aggregate(&init, &tn, n, lo, &env)
                                .unwrap_or_else(|| self.lower_expr(&init, &env)),
                            _ => self.lower_expr(&init, &env),
                        }
                    }
                    Some(TypeInfo::Vector { .. }) => {
                        // Builtin 1-D vectors (std_ulogic_vector, bit_vector,
                        // string, unsigned): named/others aggregates place
                        // elements over the decl constraint like user array
                        // types; without this the aggregate hits the generic
                        // fallback and folds to a mis-sized X (an
                        // `(OTHERS => 'U')` don't-care byte poisoned every
                        // later record field in the fold).
                        let elem = self.array_elem_width(&tn).unwrap_or(1);
                        let (n, lo) = match &constraint {
                            Some(c) => (
                                Some(self.constraint_width(c)),
                                self.range_low(c, &env).unwrap_or(0),
                            ),
                            None => (None, 0),
                        };
                        match n {
                            Some(n) if n > 0 => self
                                .lower_array_aggregate_specs(&init, n, lo, elem, None, &env)
                                .unwrap_or_else(|| self.lower_expr(&init, &env)),
                            _ => self.lower_expr(&init, &env),
                        }
                    }
                    _ => self.lower_expr(&init, &env),
                }
            }
            _ => self.lower_expr(&init, &env),
        };
        self.str_ctx = saved_str;
        if let Some(tm) = &type_mark {
            let tn = self.text(tm).to_ascii_lowercase();
            for n in &names {
                self.const_types.insert(n.to_ascii_lowercase(), tn.clone());
            }
        }
        match self.ce_env(&ex, &env) {
            Some(v) => {
                for n in &names {
                    Rc::make_mut(&mut self.consts).insert(n.to_ascii_lowercase(), v.clone());
                    // Unconstrained array constants take their shape from the
                    // value (`C_OPTS : opts_t := (f(...), f(...));` —
                    // `'length` must fold without a decl constraint).
                    self.set_obj_dims_from_init(
                        &n.to_ascii_lowercase(),
                        type_mark,
                        Some(&init),
                        &v,
                    );
                    // A constant that folded to X while its initializer hangs
                    // on an unbound formal generic (X propagates through
                    // arithmetic, so the fold "succeeds") is per-instance
                    // just like a failed fold: record it so generate ranges
                    // and other constants referencing it suppress their
                    // misleading bare-context warnings.
                    if !v.is_known() && self.refs_unset_generic(&init) {
                        self.per_instance_names.insert(n.to_ascii_lowercase());
                    }
                    let status = if v.is_known() { "ok" } else { "x" };
                    self.dump_const(n, "constant", d.span(), status, &v);
                }
            }
            None => {
                // Keep the name bound to X (e.g. a generic with no default is
                // not constant at analysis time) so uses resolve instead of
                // cascading into unknown-identifier errors.
                // An initializer hanging (directly or through an
                // already-X-derived constant) on an unbound formal generic is
                // constant per instance (the generic map rebinds it), so the
                // bare-context fold failure is not a warning — established
                // simulators evaluate these at elaboration and stay silent.
                if self.refs_unset_generic(&init) {
                    // Record these constants too: expressions referencing
                    // them (a generate range, another constant) inherit the
                    // per-instance property. The unknown-value gate in
                    // `refs_unset_generic` keeps this harmless in contexts
                    // where the generic is bound.
                    for n in &names {
                        self.per_instance_names.insert(n.to_ascii_lowercase());
                    }
                } else if !self.refs_impure_call(&ex) {
                    self.warn(
                        d.span(),
                        "constant initializer is not constant; treated as X",
                    );
                }
                for n in &names {
                    Rc::make_mut(&mut self.consts)
                        .insert(n.to_ascii_lowercase(), Value::x(width.max(1)));
                    self.dump_const(n, "constant", d.span(), "unfolded", &Value::x(width.max(1)));
                }
            }
        }
    }

    /// Function-local constant declaration. If the initializer is
    /// const-evalable it is bound in `consts` like a package-level constant.
    /// Otherwise (e.g. `constant msb := min(arg'length, size) - 1;` in
    /// std_logic_arith — valid VHDL, ModelSim accepts it) the constant gets a
    /// runtime slot and the initializer is evaluated on every call: the
    /// lowered initializer is handed back in `pending` so the caller can
    /// prepend `slot := init;` assignments to the function body. No warning:
    /// the value is legitimately dynamic, not a degraded construct.
    pub(super) fn lower_fn_local_constant(
        &mut self,
        d: &SyntaxNode,
        slots: &mut Vec<(String, usize)>,
        locals: &mut Vec<SignalIr>,
        local_types: &mut Vec<(String, String)>,
        pending: &mut Vec<(SigId, Expr)>,
    ) {
        let kids = real_children(d);
        let Some(colon) = kids
            .iter()
            .position(|k| k.kind() == SyntaxKind::Punct && self.text(k) == ":")
        else {
            return;
        };
        let names: Vec<String> = kids[..colon]
            .iter()
            .filter(|k| k.kind() == SyntaxKind::Name)
            .map(|k| self.text(k))
            .collect();
        let type_mark = kids[colon + 1..]
            .iter()
            .find(|k| k.kind() == SyntaxKind::Name);
        let constraint = kids[colon + 1..]
            .iter()
            .find(|k| k.kind() == SyntaxKind::Range);
        let eq = kids[colon + 1..]
            .iter()
            .position(|k| k.kind() == SyntaxKind::Punct && self.text(k) == "=");
        let init = match eq {
            Some(e) => kids[colon + 1 + e + 1..]
                .iter()
                .find(|k| is_expr_node(k.kind()))
                .cloned(),
            None => kids[colon + 1..]
                .iter()
                .find(|k| is_expr_node(k.kind()) && k.kind() != SyntaxKind::Name)
                .cloned(),
        };
        let env = self.consts.clone();
        let (width, signed, _, real) = type_mark
            .map(|tm| self.resolve_type(tm, constraint, d.span()))
            .unwrap_or((32, false, None, false));
        if let Some(tm) = &type_mark {
            let tn = self.text(tm).to_ascii_lowercase();
            for n in &names {
                self.const_types.insert(n.to_ascii_lowercase(), tn.clone());
                local_types.push((n.clone(), self.kw(tm)));
            }
        }
        // Inline array constraint: record the object's shape (dims +
        // directions) the same way declaration-level constants do, so
        // index/slice addressing of a local constant remaps too.
        self.record_const_shapes(type_mark, constraint, &names);
        let Some(init) = init else {
            for n in &names {
                Rc::make_mut(&mut self.consts)
                    .insert(n.to_ascii_lowercase(), Value::x(width.max(1)));
            }
            return;
        };
        let saved_str = self.str_ctx;
        self.str_ctx = type_mark.is_some_and(|tm| self.string_typed(tm));
        let ex = match type_mark {
            Some(tm) if init.kind() == SyntaxKind::Aggregate => {
                let tn = self.text(tm).to_ascii_lowercase();
                match self.types.get(&tn).cloned() {
                    Some(TypeInfo::Record { .. }) => {
                        let fields = self.record_fields_of_type(&tn);
                        if fields.is_empty() {
                            self.lower_expr(&init, &env)
                        } else {
                            self.lower_typed_record_aggregate(&init, &tn, &fields, &env)
                        }
                    }
                    Some(TypeInfo::Array { elem, .. }) => {
                        let elem = elem.max(1);
                        let (n, lo) = match constraint {
                            Some(c) => (Some(self.constraint_width(c)), {
                                self.range_low(c, &env).unwrap_or(0)
                            }),
                            None => {
                                let w = self.resolve_type(tm, None, d.span()).0;
                                self.array_agg_len(w, elem)
                            }
                        };
                        match n {
                            Some(n) if n > 0 => self
                                .lower_array_aggregate(&init, &tn, n, lo, &env)
                                .unwrap_or_else(|| self.lower_expr(&init, &env)),
                            _ => self.lower_expr(&init, &env),
                        }
                    }
                    Some(TypeInfo::Vector { .. }) => {
                        // Builtin 1-D vectors (std_ulogic_vector, bit_vector,
                        // string, unsigned): named/others aggregates place
                        // elements over the decl constraint like user array
                        // types; without this the aggregate hits the generic
                        // fallback and folds to a mis-sized X (an
                        // `(OTHERS => 'U')` don't-care byte poisoned every
                        // later record field in the fold).
                        let elem = self.array_elem_width(&tn).unwrap_or(1);
                        let (n, lo) = match &constraint {
                            Some(c) => (
                                Some(self.constraint_width(c)),
                                self.range_low(c, &env).unwrap_or(0),
                            ),
                            None => (None, 0),
                        };
                        match n {
                            Some(n) if n > 0 => self
                                .lower_array_aggregate_specs(&init, n, lo, elem, None, &env)
                                .unwrap_or_else(|| self.lower_expr(&init, &env)),
                            _ => self.lower_expr(&init, &env),
                        }
                    }
                    _ => self.lower_expr(&init, &env),
                }
            }
            _ => self.lower_expr(&init, &env),
        };
        self.str_ctx = saved_str;
        match self.ce_env(&ex, &env) {
            Some(v) => {
                for n in &names {
                    Rc::make_mut(&mut self.consts).insert(n.to_ascii_lowercase(), v.clone());
                    let status = if v.is_known() { "ok" } else { "x" };
                    self.dump_const(n, "local", d.span(), status, &v);
                }
            }
            None => {
                for n in &names {
                    let id = slots.len() as SigId;
                    slots.push((n.clone(), width.max(1)));
                    self.names.insert(n.clone(), id);
                    locals.push(SignalIr {
                        name: n.clone(),
                        width: width.max(1),
                        kind: if real { SigKind::Real } else { SigKind::Wire },
                        init: None,
                        mem: None,
                        signed,
                    });
                    // Not bound in `consts`: expression uses resolve through
                    // `names` to the runtime slot (consts are checked first in
                    // lower_expr's env), and const contexts fall back to the
                    // default width silently — the real value is computed per
                    // call.
                    pending.push((id, ex.clone()));
                    self.dump_const(n, "local", d.span(), "dynamic", &Value::x(width.max(1)));
                }
            }
        }
    }

    /// Register one protected-type method as a stub `FuncIr` (signature only,
    /// empty body) so calls `obj.method(...)` resolve at lower time.
    pub(super) fn protected_method_stub(&mut self, f: &SyntaxNode) {
        let kids = real_children(f);
        let mut name = String::new();
        let mut ret_mark: Option<SyntaxNode> = None;
        let mut param_decls: Vec<SyntaxNode> = Vec::new();
        let mut seen_return = false;
        let mut seen_begin = false;
        for k in &kids {
            match k.kind() {
                SyntaxKind::Ident if self.kw(k) == "return" => seen_return = true,
                SyntaxKind::Ident if self.kw(k) == "begin" => seen_begin = true,
                SyntaxKind::Name if !seen_return && name.is_empty() => name = self.text(k),
                SyntaxKind::Name if seen_return && !seen_begin && ret_mark.is_none() => {
                    ret_mark = Some(k.clone())
                }
                SyntaxKind::PortDecl if !seen_begin => param_decls.push(k.clone()),
                _ => {}
            }
        }
        if name.is_empty() {
            return;
        }
        let ret_width = ret_mark
            .map(|tm| self.resolve_type(&tm, None, f.span()).0)
            .unwrap_or(32);
        let mut args: Vec<(String, usize, String)> = Vec::new();
        for pd in &param_decls {
            let pk = real_children(pd);
            let colon = pk
                .iter()
                .position(|k| k.kind() == SyntaxKind::Punct && self.text(k) == ":");
            let names: Vec<String> = pk[..colon.unwrap_or(pk.len())]
                .iter()
                .filter(|k| k.kind() == SyntaxKind::Name)
                .map(|k| self.text(k))
                .collect();
            let tmark = colon
                .and_then(|c| pk.get(c + 1))
                .filter(|k| k.kind() == SyntaxKind::Name);
            let w = tmark
                .map(|tm| self.resolve_type(tm, None, pd.span()).0)
                .unwrap_or(32);
            for n in names {
                args.push((
                    n,
                    w,
                    tmark
                        .as_ref()
                        .map(|tm| self.text(tm).to_ascii_lowercase())
                        .unwrap_or_default(),
                ));
            }
        }
        self.register_func(FuncIr {
            name,
            ret_width: ret_width.max(1),
            args,
            defaults: Vec::new(),
            modes: Vec::new(),
            locals: Vec::new(),
            dyn_locals: Vec::new(),
            body: Stmt::Null,
            hidden: Vec::new(),
        });
    }
}
