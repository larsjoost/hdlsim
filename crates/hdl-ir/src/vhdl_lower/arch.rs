//! Architecture and concurrent-statement lowering: the architecture module, generate for/if/case, blocks, instance instantiation, and concurrent signal assignment dispatch.
//!
//! Extracted verbatim from vhdl_lower.rs (pure move, no logic changes).

use super::*;

impl<'a> Lower<'a> {
    /// Lower an architecture body onto its entity's module. Returns false
    /// (without emitting a diagnostic) when the entity is not in `entities` —
    /// the caller either defers the architecture or reports E0401 via
    /// [`Lower::err_unknown_entity`].
    /// Lower an architecture body. On success the body is attached to a
    /// per-architecture variant module `entity/arch` (pushed to `modules`)
    /// and the function returns `(entity, arch)`; the plain entity module gets
    /// the default architecture's body afterwards (see `apply_default_archs`).
    /// Returns None when the entity is unknown (caller defers or errors).
    pub(super) fn lower_architecture(
        &mut self,
        a: &SyntaxNode,
        modules: &mut Vec<ModuleIr>,
        entities: &HashMap<String, usize>,
    ) -> Option<(String, String)> {
        let kids = real_children(a);
        let arch_ent = arch_entity(a, self.db);
        let entity_name = arch_ent
            .as_ref()
            .map(|(_, s)| s.clone())
            .unwrap_or_default();
        let midx = *entities.get(&entity_name)?;
        // Variant re-lowering: name the arch module after the variant entity.
        let disp = self
            .variant
            .as_ref()
            .filter(|v| v.entity == entity_name.to_ascii_lowercase())
            .map(|v| format!("{}{}", entity_name.to_ascii_lowercase(), v.suffix))
            .unwrap_or_else(|| entity_name.clone());
        let idents: Vec<&SyntaxNode> = kids
            .iter()
            .filter(|k| k.kind() == SyntaxKind::Ident)
            .collect();
        let arch_name = idents
            .get(1)
            .map(|k| self.text(k).to_ascii_lowercase())
            .unwrap_or_default();
        let mut module_owned = modules[midx].clone();
        module_owned.name = format!("{disp}/{arch_name}");
        self.cur_scope = module_owned.name.clone();
        let module = &mut module_owned;
        self.names = module
            .signals
            .iter()
            .enumerate()
            .map(|(i, s)| (s.name.clone(), i as SigId))
            .collect();
        self.install_entity_maps(&entity_name);
        // Entity generics are constants in the architecture (also when the
        // architecture was deferred to a pending pass with a fresh scope).
        // A record-typed generic's type name is restored into `const_types`
        // so `G_PARAM.field` selects resolve out-of-file too (the record's
        // `TypeInfo` arrives via the entity's context clauses above).
        for prm in &modules[midx].params {
            let key = prm.name.to_ascii_lowercase();
            Rc::make_mut(&mut self.consts)
                .entry(key.clone())
                .or_insert_with(|| prm.default.clone());
            // Mirror the per-instance marker (see `lower_generic_decls`):
            // this path restores the entity's generics into a fresh Lower
            // for an out-of-file architecture, so without it the suppression
            // never sees the unbound formals.
            if self.consts.get(&key).is_some_and(|v| !v.is_known()) {
                self.per_instance_names.insert(key.clone());
            }
            if let Some(ty) = &prm.ty {
                self.const_types.entry(key).or_insert_with(|| ty.clone());
            }
        }
        // Declarative part: everything before `begin`.
        let begin_pos = kids
            .iter()
            .position(|k| k.kind() == SyntaxKind::Ident && self.kw(k) == "begin");
        let decls: Vec<&SyntaxNode> = match begin_pos {
            Some(b) => kids[..b].iter().collect(),
            None => kids.iter().collect(),
        };
        // Start from the entity declarative part's aliases (an architecture
        // sees the entity's declarative region, LRM 1076 §1.2), then add the
        // architecture's own.
        self.aliases.clear();
        self.alias_rec.clear();
        if let Some(m) = self.entity_maps.get(&entity_name) {
            self.aliases.extend(m.aliases.clone());
            self.alias_rec.extend(m.alias_rec.clone());
        }
        // Pre-register function stubs: a recursive call inside a body (or a
        // forward reference to a later-declared sibling) must lower as a
        // call, not as an index of the function-name result slot. Real
        // bodies overwrite the stub in lower_function_decl (same mechanism
        // as process-local functions).
        for d in &decls {
            if d.kind() == SyntaxKind::Function {
                if let Some(stub) = self.subprogram_stub(d) {
                    self.register_func(stub);
                }
            }
        }
        let mut config_specs: Vec<&SyntaxNode> = Vec::new();
        for d in &decls {
            // Arm the hidden-signal allocator per declarative item (decls push
            // directly into `module.signals`, so the base moves between items).
            self.extra_base = module.signals.len();
            self.extra_signals.clear();
            match d.kind() {
                SyntaxKind::SignalDecl => self.lower_signal_decl(module, d),
                SyntaxKind::ConstantDecl => {
                    self.lower_constant_decl(d);
                    self.materialize_const(module, d);
                }
                SyntaxKind::VariableDecl => self.lower_signal_decl(module, d), // rare at arch level
                SyntaxKind::TypeDecl => self.lower_type_decl(d),
                SyntaxKind::SubtypeDecl => self.lower_subtype_decl(d),
                SyntaxKind::ViewDecl => self.lower_view_decl(d),
                SyntaxKind::ComponentDecl => {} // components resolve to entities by name
                SyntaxKind::Function => self.lower_function_decl(module, d),
                SyntaxKind::AliasDecl => self.lower_alias_decl(d),
                SyntaxKind::AttrDecl => {
                    self.register_attr_decl(d);
                }
                SyntaxKind::UseClause => self.merge_use_clause(d),
                SyntaxKind::PackageInst => self.register_package_inst(d),
                SyntaxKind::FileDecl => self.lower_file_decl(d),
                // Configuration specification: `for label : comp use ...;`
                SyntaxKind::ForSpec => config_specs.push(d),
                _ => {}
            }
            for sig in self.extra_signals.drain(..) {
                module.signals.push(sig);
            }
            self.extra_base = module.signals.len();
        }
        // Rebuild names (signals added); re-apply whole-signal aliases.
        self.names = module
            .signals
            .iter()
            .enumerate()
            .map(|(i, s)| (s.name.clone(), i as SigId))
            .collect();
        for (n, a) in &self.aliases {
            if let Expr::Sig(id) = a {
                self.names.insert(n.clone(), *id);
            }
        }
        // Concurrent statements: after `begin`.
        let stmts: Vec<&SyntaxNode> = match begin_pos {
            Some(b) => kids[b + 1..].iter().collect(),
            None => Vec::new(),
        };
        let mut proc_idx = 0;
        for s in &stmts {
            self.lower_conc_stmt(module, s, &mut proc_idx);
        }
        // Protected-type methods + ctors lowered from arch-level type bodies.
        module.functions.append(&mut self.pending_funcs);
        // Configuration specifications rebind the matching instance label.
        for fs in &config_specs {
            let fk = real_children(fs);
            let Some(label) = fk
                .iter()
                .find(|k| k.kind() == SyntaxKind::Name)
                .map(|k| self.text(k))
            else {
                continue;
            };
            let Some(use_pos) = fk
                .iter()
                .position(|k| k.kind() == SyntaxKind::Ident && self.kw(k) == "use")
            else {
                continue;
            };
            let binding = self.lower_binding_kids(&fk[use_pos + 1..]);
            for inst in &mut module.instances {
                if inst.name.eq_ignore_ascii_case(&label) {
                    self.apply_binding(inst, &binding, fs.span());
                }
            }
        }
        module_owned.sig_dims_debug = Some(self.sig_dims.clone());
        modules.push(module_owned);
        Some((disp, arch_name))
    }

    /// Lower one concurrent statement (architecture body, block, or generate
    /// body — generate unrolls and re-enters here per iteration/branch).
    pub(super) fn lower_conc_stmt(
        &mut self,
        module: &mut ModuleIr,
        s: &SyntaxNode,
        proc_idx: &mut usize,
    ) {
        // Arm the hidden-signal allocator for this statement (declarations
        // inside generate/block bodies push into `module.signals` directly, so
        // the id base must be re-captured per statement).
        self.extra_base = module.signals.len();
        self.extra_signals.clear();
        match s.kind() {
            SyntaxKind::Process => {
                let p = self.lower_process(module, s, *proc_idx);
                *proc_idx += 1;
                module.processes.push(p);
            }
            SyntaxKind::ConcAssign => match self.lower_conc_assign(s, *proc_idx) {
                Some(ConcOut::Assign(a)) => module.assigns.push(a),
                Some(ConcOut::Proc(p)) => {
                    module.processes.push(p);
                    *proc_idx += 1;
                }
                None => {}
            },
            SyntaxKind::SelectedAssign => {
                if let Some(a) = self.lower_selected_assign(s) {
                    module.assigns.push(a);
                }
            }
            SyntaxKind::Inst => {
                if let Some(mut i) = self.lower_inst(s) {
                    if i.name.is_empty() {
                        i.name = format!("inst{}", self.inst_count);
                        self.inst_count += 1;
                    }
                    module.instances.push(i);
                }
            }
            SyntaxKind::AssertStmt => {
                // Concurrent assert → always-process sensitive to the
                // condition's signals; body is the sequential assert.
                let p = self.lower_conc_assert(s, *proc_idx);
                *proc_idx += 1;
                module.processes.push(p);
            }
            SyntaxKind::GenFor => self.lower_gen_for(module, s, proc_idx),
            SyntaxKind::GenIf => self.lower_gen_if(module, s, proc_idx),
            SyntaxKind::GenCase => self.lower_gen_case(module, s, proc_idx),
            SyntaxKind::Block => self.lower_block(module, s, proc_idx),
            _ => {}
        }
        for sig in self.extra_signals.drain(..) {
            module.signals.push(sig);
        }
        self.extra_base = module.signals.len();
    }

    /// Split the nodes of a generate/block body into declarative items and
    /// concurrent statements, classified by node kind (a `begin` keyword is
    /// optional in generate bodies, so position alone is not enough).
    pub(super) fn gen_body(&self, after: &[SyntaxNode]) -> (Vec<SyntaxNode>, Vec<SyntaxNode>) {
        let mut decls = Vec::new();
        let mut stmts = Vec::new();
        for k in after {
            if k.kind() == SyntaxKind::Ident {
                continue;
            }
            if is_decl_kind(k.kind()) {
                decls.push(k.clone());
            } else {
                stmts.push(k.clone());
            }
        }
        (decls, stmts)
    }

    /// Lower the declarative items of a generate/block body. When `fresh` is
    /// set (for-generate iterations) existing signal bindings are dropped
    /// first, so each iteration gets its own signal.
    pub(super) fn lower_gen_decls(
        &mut self,
        module: &mut ModuleIr,
        decls: &[SyntaxNode],
        fresh: bool,
    ) {
        // Pre-register function stubs (see the architecture declarative
        // pass): recursive calls inside a body must lower as calls, not as
        // indexes of the function-name result slot.
        for d in decls {
            if d.kind() == SyntaxKind::Function && self.subprogram_is_function(d) {
                if let Some(stub) = self.subprogram_stub(d) {
                    self.register_func(stub);
                }
            }
        }
        for d in decls {
            if fresh && matches!(d.kind(), SyntaxKind::SignalDecl | SyntaxKind::VariableDecl) {
                let dk = real_children(d);
                if let Some(colon) = dk
                    .iter()
                    .position(|k| k.kind() == SyntaxKind::Punct && self.text(k) == ":")
                {
                    for k in &dk[..colon] {
                        if k.kind() == SyntaxKind::Name {
                            self.names.remove(&self.text(k));
                        }
                    }
                }
            }
            match d.kind() {
                SyntaxKind::SignalDecl | SyntaxKind::VariableDecl => {
                    self.lower_signal_decl(module, d)
                }
                SyntaxKind::ConstantDecl => {
                    self.lower_constant_decl(d);
                    self.materialize_const(module, d);
                }
                SyntaxKind::TypeDecl => self.lower_type_decl(d),
                SyntaxKind::SubtypeDecl => self.lower_subtype_decl(d),
                SyntaxKind::ViewDecl => self.lower_view_decl(d),
                SyntaxKind::AliasDecl => self.lower_alias_decl(d),
                SyntaxKind::AttrDecl => {
                    self.register_attr_decl(d);
                }
                SyntaxKind::UseClause => self.merge_use_clause(d),
                SyntaxKind::PackageInst => self.register_package_inst(d),
                SyntaxKind::FileDecl => self.lower_file_decl(d),
                SyntaxKind::PortClause => self.lower_port_clause(module, d),
                SyntaxKind::Function if self.subprogram_is_function(d) => {
                    self.lower_function_decl(module, d)
                }
                SyntaxKind::GenericClause => {
                    // Block-header generics: register as constants so port
                    // constraints see them (not module params).
                    self.lower_generic_decls(d, &HashMap::new());
                }
                _ => {}
            }
        }
    }

    /// `for i in lo to/ downto hi generate … end generate;` — unrolled at
    /// lower time with the loop variable as a constant per iteration.
    pub(super) fn lower_gen_for(
        &mut self,
        module: &mut ModuleIr,
        s: &SyntaxNode,
        proc_idx: &mut usize,
    ) {
        let kids = real_children(s);
        let var = kids
            .iter()
            .find(|k| k.kind() == SyntaxKind::Name)
            .map(|k| self.text(k).to_ascii_lowercase());
        let gen_pos = kids
            .iter()
            .position(|k| k.kind() == SyntaxKind::Ident && self.kw(k) == "generate")
            .unwrap_or(kids.len());
        if gen_pos >= kids.len() {
            return;
        }
        let in_pos = kids
            .iter()
            .position(|k| k.kind() == SyntaxKind::Ident && self.kw(k) == "in")
            .unwrap_or(0);
        let downto = kids
            .iter()
            .any(|k| k.kind() == SyntaxKind::Ident && self.kw(k) == "downto");
        let range_nodes: Vec<&SyntaxNode> = kids
            .iter()
            .enumerate()
            .filter(|(i, k)| *i > in_pos && *i < gen_pos && is_expr_node(k.kind()))
            .map(|(_, k)| k)
            .collect();
        let env = self.consts.clone();
        let exprs: Vec<Expr> = range_nodes
            .iter()
            .map(|k| self.lower_expr(k, &env))
            .collect();
        if exprs.len() < 2 {
            return;
        }
        let ev = |e: &Expr| self.ce_env(e, &env).and_then(|v| v.to_u64());
        let (Some(lo), Some(hi)) = (ev(&exprs[0]), ev(&exprs[1])) else {
            // A range hanging on an unbound formal generic is constant per
            // instance (the generic map rebinds it) — same reasoning as the
            // constant-initializer suppression.
            if !range_nodes.iter().any(|k| self.refs_unset_generic(k)) {
                self.warn(s.span(), "generate range is not constant; skipped");
            }
            return;
        };
        let (decls, body) = self.gen_body(&kids[gen_pos + 1..]);
        // Generate-local declarations and the loop parameter are scoped to
        // the generate (LRM 11.2): snapshot the const map so a constant
        // declared inside the generate does not leak into statements
        // lowered after it.
        let saved_consts = self.consts.clone();
        // Bounds may be real-typed (invalid VHDL) whose f64 bit pattern reads
        // as a huge u64; check the iteration count before allocating.
        let n = if downto {
            lo.checked_sub(hi)
        } else {
            hi.checked_sub(lo)
        }
        .and_then(|d| d.checked_add(1));
        let Some(n) = n.filter(|&n| n <= 1 << 20) else {
            self.warn(
                s.span(),
                "generate loop range is not a constant discrete range (or too large); skipped",
            );
            return;
        };
        if crate::debug::TRACE_GENRANGE.load(std::sync::atomic::Ordering::Relaxed) && n > 64 {
            let sf = self.db.file(s.span().file);
            let (line, _col) = sf.line_col(s.span().start);
            eprintln!(
                "genrange lo={} hi={} downto={} n={} {}:{} lo_expr {:.80?} hi_expr {:.80?}",
                lo,
                hi,
                downto,
                n,
                sf.name(),
                line,
                exprs[0],
                exprs[1],
            );
        }
        let vals: Vec<u64> = if downto {
            (hi..=lo).rev().take(n as usize).collect()
        } else {
            (lo..=hi).take(n as usize).collect()
        };
        for v in vals {
            if let Some(var) = &var {
                Rc::make_mut(&mut self.consts).insert(var.clone(), Value::from_u64(32, v));
            }
            self.lower_gen_decls(module, &decls, true);
            for k in &body {
                self.lower_conc_stmt(module, k, proc_idx);
            }
        }
        self.consts = saved_consts;
    }

    /// `if cond generate … [elsif …] [else …] end if generate;` — the first
    /// branch whose condition is constant-true is lowered.
    pub(super) fn lower_gen_if(
        &mut self,
        module: &mut ModuleIr,
        s: &SyntaxNode,
        proc_idx: &mut usize,
    ) {
        let kids = real_children(s);
        // Split into branches at if/elsif/else. Within a branch the condition
        // comes before `generate`, declarations between `generate` and
        // `begin`, and statements after `begin`.
        struct Seg {
            kw: String,
            cond: Option<SyntaxNode>,
            decls: Vec<SyntaxNode>,
            stmts: Vec<SyntaxNode>,
        }
        let mut segs: Vec<Seg> = Vec::new();
        let mut seen_generate = false;
        for k in &kids {
            let w = if k.kind() == SyntaxKind::Ident {
                self.kw(k)
            } else {
                String::new()
            };
            match w.as_str() {
                "if" | "elsif" | "else" => {
                    segs.push(Seg {
                        kw: w,
                        cond: None,
                        decls: Vec::new(),
                        stmts: Vec::new(),
                    });
                    seen_generate = false;
                }
                "generate" => seen_generate = true,
                _ => {
                    if let Some(seg) = segs.last_mut() {
                        if !seen_generate && seg.cond.is_none() && is_expr_node(k.kind()) {
                            seg.cond = Some(k.clone());
                        } else if k.kind() != SyntaxKind::Ident {
                            if is_decl_kind(k.kind()) {
                                seg.decls.push(k.clone());
                            } else {
                                seg.stmts.push(k.clone());
                            }
                        }
                    }
                }
            }
        }
        for seg in segs {
            let take = if seg.kw == "else" {
                true
            } else {
                seg.cond
                    .map(|c| {
                        let env = self.consts.clone();
                        let e = self.lower_expr(&c, &env);
                        self.ce_env(&e, &env)
                            .and_then(|v| v.to_u64())
                            .map(|n| n != 0)
                            .unwrap_or(false)
                    })
                    .unwrap_or(false)
            };
            if take {
                self.lower_scoped(module, &seg.decls, &seg.stmts, proc_idx);
                return;
            }
        }
    }

    /// `case sel generate when c => … [when others => …] end case generate;`
    /// — first matching alternative is lowered.
    pub(super) fn lower_gen_case(
        &mut self,
        module: &mut ModuleIr,
        s: &SyntaxNode,
        proc_idx: &mut usize,
    ) {
        let kids = real_children(s);
        let env = self.consts.clone();
        let sel_pos = kids
            .iter()
            .position(|k| is_expr_node(k.kind()))
            .unwrap_or(kids.len());
        let sel = self.lower_expr(&kids[sel_pos], &env);
        let sel_v = self.ce_env(&sel, &env).and_then(|v| v.to_u64());
        let items: Vec<&SyntaxNode> = kids
            .iter()
            .filter(|k| k.kind() == SyntaxKind::CaseItem)
            .collect();
        let mut others: Option<(Vec<SyntaxNode>, Vec<SyntaxNode>)> = None;
        for item in items {
            let ik = real_children(item);
            let has_others = ik.iter().any(|k| k.kind() == SyntaxKind::Others);
            let choices: Vec<Expr> = ik
                .iter()
                .filter(|k| is_expr_node(k.kind()) && k.kind() != SyntaxKind::Others)
                .map(|k| self.lower_expr(k, &env))
                .collect();
            let arrow = ik
                .iter()
                .position(|k| k.kind() == SyntaxKind::Punct && self.text(k) == "=")
                .unwrap_or(ik.len());
            let after_arrow = if arrow < ik.len() {
                &ik[arrow + 1..]
            } else {
                &ik[arrow..]
            };
            let (decls, stmts) = self.gen_body(after_arrow);
            let matches = has_others
                || sel_v
                    .map(|sv| {
                        choices.iter().any(|c| {
                            self.ce_env(c, &env)
                                .and_then(|v| v.to_u64())
                                .map(|cv| cv == sv)
                                .unwrap_or(false)
                        })
                    })
                    .unwrap_or(false);
            if matches {
                self.lower_scoped(module, &decls, &stmts, proc_idx);
                return;
            }
            if has_others {
                others = Some((decls, stmts));
            }
        }
        if let Some((decls, stmts)) = others {
            self.lower_scoped(module, &decls, &stmts, proc_idx);
        }
    }

    /// Lower declarative items + concurrent statements in a NEW lexical scope
    /// (blocks and generate alternatives, LRM 11.2): a block-local signal
    /// SHADOWS an outer declaration of the same name instead of being dropped
    /// by the duplicate guard, and is invisible after the scope ends. The
    /// declarations still become signals/constants of the enclosing module —
    /// only the name bindings are scoped.
    pub(super) fn lower_scoped(
        &mut self,
        module: &mut ModuleIr,
        decls: &[SyntaxNode],
        stmts: &[SyntaxNode],
        proc_idx: &mut usize,
    ) {
        let saved = std::mem::take(&mut self.names);
        self.names_stack.push(saved);
        // Block/generate declarations are lexically scoped (LRM 11.2): a
        // block-local `CONSTANT C_DATA_WIDTH : positive := 1;` must not
        // shadow the enclosing architecture's same-named constant for the
        // statements lowered *after* the block, so snapshot the const map
        // and restore it when the scope ends.
        let saved_consts = self.consts.clone();
        self.lower_gen_decls(module, decls, false);
        for st in stmts {
            self.lower_conc_stmt(module, st, proc_idx);
        }
        self.consts = saved_consts;
        self.names = self.names_stack.pop().unwrap_or_default();
    }

    /// `block [is] decls begin stmts end block;` — decls and statements are
    /// lowered into the enclosing module; the declarations are lexically
    /// scoped to the block (see `lower_scoped`).
    pub(super) fn lower_block(
        &mut self,
        module: &mut ModuleIr,
        s: &SyntaxNode,
        proc_idx: &mut usize,
    ) {
        let kids = real_children(s);
        let (decls, stmts) = self.gen_body(&kids);
        // Block-header ports are handled by the PortClause arm in
        // lower_gen_decls: they become signals of the enclosing module.
        self.lower_scoped(module, &decls, &stmts, proc_idx);
    }

    /// E0401 for an architecture naming an entity that is unknown design-wide:
    /// the caret points at the entity name itself (not the whole architecture
    /// node), with a did-you-mean help when a known entity name is close.
    pub(super) fn err_unknown_entity(&mut self, a: &SyntaxNode, candidates: &[String]) {
        let arch_ent = arch_entity(a, self.db);
        let entity_name = arch_ent
            .as_ref()
            .map(|(_, s)| s.clone())
            .unwrap_or_default();
        let span = arch_ent
            .as_ref()
            .map(|(n, _)| n.span())
            .unwrap_or_else(|| a.span());
        let help =
            did_you_mean(&entity_name, candidates.iter()).map(|s| format!("did you mean '{s}'?"));
        self.diags.push(Diagnostic {
            code: "E0401",
            severity: Severity::Error,
            span: Some(span),
            message: format!("architecture for unknown entity '{entity_name}'"),
            help,
        });
    }

    pub(super) fn lower_inst(&mut self, s: &SyntaxNode) -> Option<InstIr> {
        let kids = real_children(s);
        // Instance label: `u :` — an Ident child right before the `:` punct
        // (the parser wraps `label :` into the Inst node).
        let label = kids
            .iter()
            .position(|k| k.kind() == SyntaxKind::Punct && self.text(k) == ":")
            .and_then(|p| p.checked_sub(1).and_then(|i| kids.get(i)))
            .filter(|k| k.kind() == SyntaxKind::Ident)
            .map(|k| self.text(k));
        // `label : configuration lib.cfg ...` — the unit is a configuration.
        let is_config = kids
            .iter()
            .any(|k| k.kind() == SyntaxKind::Ident && self.kw(k) == "configuration");
        // `entity lib.unit(arch)` parses as Call(Name "lib.unit", "(", Name
        // "arch", ")"); without an arch it is a plain Name child.
        let (unit_text, arch) = match kids.iter().find(|k| k.kind() == SyntaxKind::Call) {
            Some(call) => {
                let ck = real_children(call);
                let un = ck
                    .iter()
                    .find(|k| k.kind() == SyntaxKind::Name)
                    .map(|k| self.text(k))?;
                let arch = ck
                    .iter()
                    .position(|k| k.kind() == SyntaxKind::Punct && self.text(k) == "(")
                    .and_then(|p| ck.get(p + 1))
                    .filter(|k| k.kind() == SyntaxKind::Name)
                    .map(|k| self.text(k).to_ascii_lowercase());
                (un, arch)
            }
            None => (
                kids.iter()
                    .find(|k| k.kind() == SyntaxKind::Name)
                    .map(|k| self.text(k))?,
                None,
            ),
        };
        // `entity work.counter` — drop the library prefix; the unit name is
        // the last selected component. With `(arch)`, select the `unit/arch`
        // variant module (see lower_architecture). A `configuration work.cfg`
        // instantiation resolves through the design-wide configuration map.
        let unit = unit_text.rsplit('.').next().unwrap_or("").to_string();
        let mut config = None;
        let name = if is_config {
            let key = unit.to_ascii_lowercase();
            match self.cfgs.get(&key).cloned() {
                Some(cfg) => {
                    config = Some(key);
                    match &cfg.arch {
                        Some(a) => format!("{}/{}", cfg.entity, a),
                        None => cfg.entity.clone(),
                    }
                }
                None => {
                    self.err(
                        kids.iter()
                            .find(|k| k.kind() == SyntaxKind::Name)
                            .map(|k| k.span())
                            .unwrap_or(s.span()),
                        format!("unknown configuration '{unit}'"),
                    );
                    unit
                }
            }
        } else {
            match arch {
                Some(a) => format!("{unit}/{a}"),
                None => unit,
            }
        };
        let mut param_overrides = Vec::new();
        let mut conns = Vec::new();
        let mut in_generic = false;
        for k in &kids {
            match k.kind() {
                SyntaxKind::Ident => {
                    let w = self.kw(k);
                    if w == "generic" {
                        in_generic = true;
                    } else if w == "port" {
                        in_generic = false;
                    }
                }
                SyntaxKind::PortConn => {
                    let ck = real_children(k);
                    // A `=>` arrow marks a named association; in a positional
                    // one the leading Name is the actual itself, not a formal —
                    // treating it as a formal would drop the connection.
                    let name_pos = self.assoc_formal(&ck);
                    // Indexed formal `i_data(0 to 1) => x`: keep the base name.
                    let assoc_name = name_pos.map(|i| {
                        let t = self.text(&ck[i]);
                        let t = t.rsplit('.').next().unwrap_or(t.as_str());
                        let end = t.find('(').unwrap_or(t.len());
                        t[..end].to_string()
                    });
                    let expr = ck
                        .iter()
                        .enumerate()
                        .rev()
                        .find(|(i, c)| Some(*i) != name_pos && is_expr_node(c.kind()))
                        .map(|(_, c)| c);
                    let Some(e) = expr else { continue };
                    let mut ex = self.lower_expr(e, &self.consts.clone());
                    if in_generic {
                        ex = self.fold_generic_actual(ex);
                    }
                    match (assoc_name, in_generic) {
                        (Some(n), true) => param_overrides.push((Some(n), ex)),
                        (Some(n), false) => conns.push(Conn::Named(n, ex)),
                        // Positional generic actuals bind to the target's
                        // generics in declaration order; the name is
                        // resolved at elaboration, where the entity's
                        // parameter list is visible
                        // (resolve_positional_params).
                        (None, true) => param_overrides.push((None, ex)),
                        (None, false) => conns.push(Conn::Positional(ex)),
                    }
                }
                _ => {}
            }
        }
        Some(InstIr {
            module: name,
            name: label.unwrap_or_default(),
            param_overrides,
            conns,
            config,
        })
    }
}
