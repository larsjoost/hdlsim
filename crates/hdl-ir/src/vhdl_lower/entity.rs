//! Entity lowering: the entity module, generic clauses/decls, and the port clause.
//!
//! Extracted verbatim from vhdl_lower.rs (pure move, no logic changes).

use super::*;

impl<'a> Lower<'a> {
    pub(super) fn lower_entity(&mut self, e: &SyntaxNode) -> ModuleIr {
        let kids = real_children(e);
        let name = kids
            .iter()
            .find(|k| {
                k.kind() == SyntaxKind::Ident
                    && !matches!(self.kw(k).as_str(), "entity" | "is" | "end" | "begin")
            })
            .map(|k| self.text(k))
            .unwrap_or_else(|| "<anon>".to_string());
        let mut module = ModuleIr {
            name,
            params: Vec::new(),
            ports: Vec::new(),
            signals: Vec::new(),
            assigns: Vec::new(),
            processes: Vec::new(),
            functions: Vec::new(),
            instances: Vec::new(),
            time_unit_fs: 1_000_000,
            is_vhdl: true,
            real_ranges: HashMap::new(),
            sig_dims_debug: None,
        };
        self.names.clear();
        self.cur_entity = module.name.to_ascii_lowercase();
        self.cur_scope = module.name.clone();
        // Snapshot scope so the entity declarative part's constants/types can
        // be exported to the entity's architectures (SigMaps).
        let consts_before: std::collections::HashSet<String> =
            self.consts.keys().cloned().collect();
        let types_before: std::collections::HashSet<String> = self.types.keys().cloned().collect();
        let aliases_before: std::collections::HashSet<String> =
            self.aliases.keys().cloned().collect();
        let alias_rec_before: std::collections::HashSet<String> =
            self.alias_rec.keys().cloned().collect();
        let obj_dims_before: std::collections::HashSet<String> =
            self.obj_dims.keys().cloned().collect();
        for k in &kids {
            // Arm the hidden-signal allocator for this phase (ports/generics
            // push directly into `module.signals`, so re-arm per item).
            self.extra_base = module.signals.len();
            self.extra_signals.clear();
            match k.kind() {
                SyntaxKind::GenericClause => self.lower_generic_clause(&mut module, k),
                SyntaxKind::PortClause => self.lower_port_clause(&mut module, k),
                SyntaxKind::ConstantDecl => self.lower_constant_decl(k),
                // Entity-level signals/variables are real module signals: an
                // architecture sees the entity's declarative region, so e.g.
                // `signal TransRec : RecType(...);` in the entity is usable
                // (and its record member selects resolve) in every
                // architecture of the entity.
                SyntaxKind::SignalDecl => self.lower_signal_decl(&mut module, k),
                SyntaxKind::VariableDecl => self.lower_signal_decl(&mut module, k),
                SyntaxKind::TypeDecl => self.lower_type_decl(k),
                SyntaxKind::SubtypeDecl => self.lower_subtype_decl(k),
                SyntaxKind::ViewDecl => self.lower_view_decl(k),
                SyntaxKind::AliasDecl => self.lower_alias_decl(k),
                SyntaxKind::AttrDecl => {
                    self.register_attr_decl(k);
                }
                SyntaxKind::UseClause => self.merge_use_clause(k),
                SyntaxKind::PackageInst => self.register_package_inst(k),
                SyntaxKind::FileDecl => self.lower_file_decl(k),
                _ => {}
            }
            for sig in self.extra_signals.drain(..) {
                module.signals.push(sig);
            }
            self.extra_base = module.signals.len();
        }
        // Rebuild names from signals (ports registered above).
        self.names = module
            .signals
            .iter()
            .enumerate()
            .map(|(i, s)| (s.name.clone(), i as SigId))
            .collect();
        self.entity_consts = self
            .consts
            .iter()
            .filter(|(k, _)| !consts_before.contains(*k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        self.entity_types = self
            .types
            .iter()
            .filter(|(k, _)| !types_before.contains(*k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        self.entity_obj_dims = self
            .obj_dims
            .iter()
            .filter(|(k, _)| !obj_dims_before.contains(*k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        self.entity_aliases = self
            .aliases
            .iter()
            .filter(|(k, _)| !aliases_before.contains(*k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        self.entity_alias_rec = self
            .alias_rec
            .iter()
            .filter(|(k, _)| !alias_rec_before.contains(*k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        // Snapshot the dims the lowerer believed for this unit BEFORE
        // snapshot_entity_maps clears the per-signal maps (the arch path
        // re-installs the entity's maps via install_entity_maps).
        module.sig_dims_debug = Some(self.sig_dims.clone());
        self.snapshot_entity_maps(&module.name);
        if let Some(v) = &self.variant {
            if v.entity == module.name.to_ascii_lowercase() {
                module.name = format!("{}{}", module.name, v.suffix);
            }
        }
        module
    }

    pub(super) fn lower_generic_clause(&mut self, module: &mut ModuleIr, g: &SyntaxNode) {
        let overrides = self
            .variant
            .as_ref()
            .filter(|v| v.entity == self.cur_entity)
            .map(|v| v.overrides.clone())
            .unwrap_or_default();
        for (name, def, ty) in self.lower_generic_decls(g, &overrides) {
            module.params.push(ParamIr {
                name,
                default: def,
                ty,
            });
        }
    }

    /// Lower the `GenericDecl`s of a generic clause into `consts` (so later
    /// declarations see the generic names) and return `(name, value)` per
    /// generic. `overrides` (from a `generic map` at a package instantiation)
    /// replaces the declared default values.
    pub(super) fn lower_generic_decls(
        &mut self,
        g: &SyntaxNode,
        overrides: &HashMap<String, Value>,
    ) -> Vec<(String, Value, Option<String>)> {
        let mut out = Vec::new();
        for d in real_children(g)
            .into_iter()
            .filter(|k| k.kind() == SyntaxKind::GenericDecl)
        {
            // `name [, name...] : type_mark [constraint] [:= default]` — the
            // declared names are the Name children before the first `:`.
            let dk = real_children(&d);
            let colon = dk
                .iter()
                .position(|k| k.kind() == SyntaxKind::Punct && self.text(k) == ":");
            let (names, after) = match colon {
                Some(c) => (
                    dk[..c]
                        .iter()
                        .filter(|k| k.kind() == SyntaxKind::Name)
                        .map(|k| self.text(k))
                        .collect::<Vec<_>>(),
                    &dk[c + 1..],
                ),
                None => (
                    dk.iter()
                        .filter(|k| k.kind() == SyntaxKind::Name)
                        .map(|k| self.text(k))
                        .collect::<Vec<_>>(),
                    &dk[..],
                ),
            };
            let type_mark = self.iface_type_mark(after);
            let constraint = after.iter().find(|k| k.kind() == SyntaxKind::Range);
            let eq = after
                .iter()
                .position(|k| k.kind() == SyntaxKind::Punct && self.text(k) == "=");
            let default = eq
                .and_then(|e| {
                    after[e + 1..]
                        .iter()
                        .find(|k| is_expr_node(k.kind()))
                        .cloned()
                })
                .or_else(|| {
                    after
                        .iter()
                        .find(|k| is_expr_node(k.kind()) && k.kind() != SyntaxKind::Name)
                        .cloned()
                });
            // Generic *type* parameter (box): `type ExpectedType;` or the
            // `T : type` form (the parser reads the `type` keyword as the
            // type mark) — register an opaque placeholder so uses of the
            // name resolve.
            let is_box = dk
                .iter()
                .any(|k| k.kind() == SyntaxKind::Ident && self.kw(k) == "type")
                || after
                    .iter()
                    .find(|k| k.kind() == SyntaxKind::Name)
                    .is_some_and(|k| self.kw(k) == "type");
            if is_box {
                for n in &names {
                    self.types.insert(n.to_ascii_lowercase(), TypeInfo::Scalar);
                }
                continue;
            }
            let span = d.span();
            let (width, signed, _, _) = match type_mark {
                Some(tm) => self.resolve_type(tm, constraint, span),
                None => (32, false, None, false),
            };
            let def = default
                .as_ref()
                .and_then(|e| {
                    let saved = self.str_ctx;
                    self.str_ctx = type_mark.as_ref().is_some_and(|tm| self.string_typed(tm));
                    let ex = self.lower_expr(e, &HashMap::new());
                    self.str_ctx = saved;
                    self.ce_env(&ex, &HashMap::new())
                })
                .unwrap_or(Value::x(width.max(1)));
            let _ = (width, signed);
            // Record type of a record-typed generic, resolved (qualified
            // `pkg.rec_t` marks reduce to the bare registered name) so field
            // selects can find it in `types` — in this scope and, via
            // `ParamIr.ty`, in the architecture's fresh scope.
            let rec_ty = type_mark
                .as_ref()
                .and_then(|tm| self.as_record(&self.kw(tm)));
            // Array dims of a multidimensional generic: from its own decl
            // constraint, from a constrained constant named as the default,
            // or from the shape of an aggregate default (`'length(d)` on the
            // generic must fold in the architecture without the actual's
            // type — overrides carry values, not constraints).
            let mut gen_dims: Option<Vec<(i64, usize)>> = None;
            if let (Some(tm), Some(c)) = (&type_mark, &constraint) {
                let tn = self
                    .kw(tm)
                    .rsplit('.')
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if matches!(self.types.get(&tn), Some(TypeInfo::Array { .. }))
                    && !self.text(c).contains("<>")
                {
                    let cd = self.constraint_dims(c);
                    if !cd.is_empty() {
                        gen_dims = Some(cd);
                    }
                }
            }
            if gen_dims.is_none() {
                if let Some(d) = &default {
                    match d.kind() {
                        SyntaxKind::Name => {
                            let dn = self
                                .kw(d)
                                .rsplit('.')
                                .next()
                                .unwrap_or("")
                                .to_ascii_lowercase();
                            gen_dims = self.obj_dims.get(&dn).cloned();
                        }
                        SyntaxKind::Aggregate => {
                            let ad = self.aggregate_dims(d);
                            if ad.len() > 1 {
                                gen_dims = Some(ad);
                            }
                        }
                        // Literal default on an unconstrained array type: the
                        // packed value carries the shape (`G_F : string := ""`
                        // folds `'length` to 0, closing file-name branches).
                        _ => gen_dims = self.dims_from_init(type_mark, Some(d), &def),
                    }
                }
            }
            for n in &names {
                let key = n.to_ascii_lowercase();
                let val = overrides.get(&key).cloned().unwrap_or_else(|| def.clone());
                Rc::make_mut(&mut self.consts).insert(key.clone(), val.clone());
                if !val.is_known() {
                    // Insert-only marker: instance contexts re-lower the same
                    // generic with a known value, and the suppression check
                    // gates on the const being unknown right now, so stale
                    // entries from other contexts stay harmless.
                    self.per_instance_names.insert(key.clone());
                }
                let status = if val.is_known() { "ok" } else { "unset" };
                self.dump_const(n, "generic", d.span(), status, &val);
                if let Some(tn) = &rec_ty {
                    self.const_types.insert(key.clone(), tn.clone());
                }
                if let Some(gd) = &gen_dims {
                    // Default-derived dims describe the default's shape only
                    // while the generic keeps it: an override may bind a
                    // differently shaped actual (values carry no shape), and
                    // a wrong dims entry would fold `'length(d)` to a wrong
                    // number instead of X.
                    if !overrides.contains_key(&key) {
                        self.obj_dims.insert(key.clone(), gd.clone());
                    }
                }
                if overrides.contains_key(&key) {
                    // An overridden generic: the actual's packed value still
                    // carries one-dimensional shape, so fold dims from it —
                    // `'length` must describe the actual, not the default. An
                    // unknown-shaped actual clears any stale default-derived
                    // entry: `'length` folds to X, never to a wrong number.
                    match self.dims_from_init(type_mark, None, &val) {
                        Some(dims) => {
                            self.obj_dims.insert(key.clone(), dims);
                        }
                        None => {
                            self.obj_dims.remove(&key);
                        }
                    }
                }
                out.push((n.clone(), val, rec_ty.clone()));
            }
        }
        out
    }

    pub(super) fn lower_port_clause(&mut self, module: &mut ModuleIr, p: &SyntaxNode) {
        for d in real_children(p)
            .into_iter()
            .filter(|k| k.kind() == SyntaxKind::PortDecl)
        {
            let dk = real_children(&d);
            let mut names = Vec::new();
            let mut mode = "in".to_string();
            let mut type_mark = None;
            let mut constraint = None;
            for k in &dk {
                match k.kind() {
                    // Mode keywords (`in`/`out`/…) are keyword tokens (Ident),
                    // not Name nodes — check them here or every port stays `in`.
                    SyntaxKind::Ident => {
                        let t = self.kw(k);
                        if matches!(t.as_str(), "in" | "out" | "inout" | "buffer") {
                            mode = t;
                        }
                    }
                    SyntaxKind::Name => names.push(self.text(k)),
                    SyntaxKind::Range => constraint = Some(k.clone()),
                    _ => {}
                }
            }
            // The type mark is the first Name child after the ':' punct
            // (skipping a `view V of` prefix).
            if let Some(pos) = dk
                .iter()
                .position(|k| k.kind() == SyntaxKind::Punct && self.text(k) == ":")
            {
                type_mark = self.iface_type_mark(&dk[pos + 1..]).cloned();
                // Port names are Name children before the ':'.
                names = dk[..pos]
                    .iter()
                    .filter(|k| k.kind() == SyntaxKind::Name)
                    .map(|k| self.text(k))
                    .collect();
                // ...but the mode keyword was also before ':' and is an Ident, not a Name.
            }
            // Port default (`:= expr` after the type mark): const-eval it into
            // the port signal's init, so an unconnected (`open`) in-port still
            // gets its default value at elaboration.
            let init_v = dk
                .iter()
                .position(|k| k.kind() == SyntaxKind::Punct && self.text(k) == ":")
                .and_then(|pos| {
                    dk[pos + 1..]
                        .iter()
                        .find(|k| is_expr_node(k.kind()) && k.kind() != SyntaxKind::Name)
                })
                .and_then(|e| {
                    let ex = self.lower_expr(e, &self.consts.clone());
                    self.ce(&ex)
                });
            let span = d.span();
            let (width, signed, mem, _real) = match &type_mark {
                Some(tm) => self.resolve_type(tm, constraint.as_ref(), span),
                None => (1, false, None, false),
            };
            let width = self.mem_row_width(type_mark.as_ref(), width);
            let dir = match mode.as_str() {
                "out" | "buffer" => Dir::Out,
                "inout" => Dir::Inout,
                _ => Dir::In,
            };
            let kind = match dir {
                Dir::In => SigKind::Input,
                Dir::Out => SigKind::Output,
                Dir::Inout => SigKind::Inout,
            };
            for n in &names {
                let id = module.signals.len() as SigId;
                if constraint.is_none()
                    && (mem.is_some()
                        || matches!(
                            type_mark
                                .as_ref()
                                .map(|tm| self.text(tm).to_ascii_lowercase())
                                .as_deref(),
                            Some(
                                "std_ulogic_vector"
                                    | "std_logic_vector"
                                    | "unsigned"
                                    | "signed"
                                    | "bit_vector"
                            )
                        ))
                {
                    self.uncon_ports.insert(id);
                }
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
                self.record_obj_shape(id, type_mark.as_ref(), constraint.as_ref());
                // Block-header ports must be visible to the block body (entity
                // ports get their names map rebuilt by lower_architecture, but
                // a block header adds signals mid-body).
                self.names.insert(n.clone(), id);
                if let Some(tm) = &type_mark {
                    let tn = self.text(tm).to_ascii_lowercase();
                    if let Some(rn) = self.as_record(&tn) {
                        self.sig_rec.insert(id, rn.clone());
                        module
                            .real_ranges
                            .insert(n.clone(), self.record_real_ranges(&rn));
                    }
                    if let Some(en) = self.as_elem_record(&tn) {
                        self.sig_elem_rec.insert(id, en);
                    }
                    if let Some(TypeInfo::Array { dims, .. }) = self.types.get(&tn) {
                        if !dims.is_empty() {
                            let d = match &constraint {
                                Some(c) => self.constraint_dims(c),
                                None => dims.clone(),
                            };
                            self.sig_dims.insert(id, d);
                        } else if let Some(td) = self.type_dims.get(&tn).cloned() {
                            // Vector subtype carrying its own range
                            // (`subtype r is slv(0 to W-1)`).
                            self.sig_dims.insert(id, td);
                        } else if let Some(c) = &constraint {
                            // Unconstrained array base with an inline object
                            // constraint: the dims belong to the object (see
                            // the signal-decl site for the 1-D caveat).
                            if !self.text(c).contains("<>") {
                                let cd = self.constraint_dims(c);
                                if !cd.is_empty() {
                                    self.set_sig_dims_from_constraint(id, cd, n);
                                }
                            }
                        }
                    } else if let Some(td) = self.type_dims.get(&tn).cloned() {
                        self.sig_dims.insert(id, td);
                    }
                }
                module.ports.push(PortIr {
                    name: n.clone(),
                    dir,
                    sig: id,
                });
            }
        }
    }
}
