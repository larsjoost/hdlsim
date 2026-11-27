//! Package lowering and scope merging: FuncIr registration, package merge (transitive use-clause expansion), package instantiations, and package body/spec lowering.
//!
//! Extracted verbatim from vhdl_lower.rs (pure move, no logic changes).

use super::*;

impl<'a> Lower<'a> {
    /// Merge a package's items into scope: all of them, or only `item`.
    /// Built-in type marks win (`or_insert`), so e.g. `std_logic` keeps the
    /// 1-bit scalar model even though ieee.std_logic_1164 declares it as a
    /// resolved 9-state subtype.
    /// With `force`, an instantiated package's items override existing ones
    /// (e.g. the generic package's own defaults that leaked into scope while
    /// it was lowered); otherwise built-ins and earlier bindings win.
    /// Register a lowered function/procedure. Overloads (same name, other
    /// signatures) coexist as separate candidates. A candidate with the same
    /// signature (formal count and widths) replaces the earlier one: package
    /// bodies follow specs and real bodies follow pre-registered stubs, and
    /// the body is the one that must execute.
    pub(super) fn register_func(&mut self, f: FuncIr) {
        let cands = self.funcs.entry(f.name.to_ascii_lowercase()).or_default();
        let sig = |c: &FuncIr| {
            c.args.len() == f.args.len()
                && c.args.iter().map(|a| a.1).eq(f.args.iter().map(|a| a.1))
        };
        let is_stub = |c: &FuncIr| crate::ir::is_bodiless(&c.body);
        // A spec stub followed by its body: the body replaces the stub and
        // inherits the spec's interface defaults (`w : natural := 8`); a
        // stub never displaces a real body. Two REAL bodies with the same
        // formal widths are genuine overloads the width model cannot
        // separate (`round(signed)` vs `round(std_ulogic_vector)`, both
        // w1) — they must coexist, or a type-conversion call inside one of
        // them (`round(signed(arg))` in the suv body) resolves back to the
        // executing function itself and recurses forever.
        // Stub matching is by name + arity, not formal widths: the stub's
        // widths are placeholders (`subprogram_stub` registers every formal
        // at 32), so a width-equality match never fires for a body whose
        // formal resolves differently (`string` → 8) — the placeholder stub
        // stayed registered beside its own body and dispatch matched an
        // actual to the placeholder width, const-evaluating the call to X
        // (every `f("abc", n)`-shaped constant initializer).
        if is_stub(&f)
            && cands
                .iter()
                .any(|c| c.args.len() == f.args.len() && !is_stub(c))
        {
            return;
        }
        if crate::debug::TRACE_OVERLOAD.load(std::sync::atomic::Ordering::Relaxed)
            && !is_stub(&f)
            && cands.iter().any(|c| sig(c) && !is_stub(c))
        {
            let span_of = |c: &FuncIr| match &c.body {
                Stmt::Spanned { span, .. } => Some((span.file.0, span.start)),
                _ => None,
            };
            eprintln!(
                "overload {:?} arity={} widths {:?} spans {:?}",
                f.name,
                f.args.len(),
                f.args.iter().map(|a| a.1).collect::<Vec<_>>(),
                cands
                    .iter()
                    .filter(|c| sig(c) && !is_stub(c))
                    .filter_map(span_of)
                    .chain(span_of(&f))
                    .collect::<Vec<_>>(),
            );
        }
        // Same placeholder-width rule for the replacement: find the stub by
        // name + arity so the body takes over the spec's slot even when the
        // resolved formal widths differ from the stub's placeholders.
        match cands
            .iter()
            .position(|c| c.args.len() == f.args.len() && is_stub(c))
        {
            Some(i) => {
                // The spec's interface carries the formals' defaults, the
                // body's capture usually records none — keep them when the
                // replacement records none.
                let mut f = f;
                if !f.defaults.iter().any(|d| d.is_some())
                    && cands[i].defaults.iter().any(|d| d.is_some())
                {
                    f.defaults = cands[i].defaults.clone();
                }
                cands[i] = f;
            }
            None => cands.push(f),
        }
    }

    pub(super) fn merge_package(&mut self, p: &PackageIr, item: Option<&str>, force: bool) {
        let mut visited = std::mem::take(&mut self.merged_all);
        self.merge_package_rec(p, item, force, &mut visited);
        self.merged_all = visited;
    }

    pub(super) fn merge_package_rec(
        &mut self,
        p: &PackageIr,
        item: Option<&str>,
        force: bool,
        visited: &mut std::collections::HashSet<String>,
    ) {
        // Bulk-merge (`use lib.pkg.all`) of a package already merged into this
        // scope is idempotent — skip it (the transitive closure was merged
        // with it). Item-specific and forced merges always run.
        if item.is_none() && !force && visited.contains(&p.name.to_ascii_lowercase()) {
            return;
        }
        // `use pkg.enum_type;` makes the type's enumeration literals directly
        // visible along with the type mark (LRM 10.4 applied to a type mark;
        // matches Questa) — collect them so the const loop below merges them.
        let mut lit_ok: Option<std::collections::HashSet<String>> = None;
        if let Some(i) = item {
            for (tname, tinfo) in &p.types {
                if tname == i {
                    if let TypeInfo::Enum(lits) = tinfo {
                        lit_ok = Some(lits.iter().cloned().collect());
                    }
                    break;
                }
            }
        }
        for (tname, tinfo) in &p.types {
            if item.is_none_or(|i| i == tname) {
                if force {
                    self.types.insert(tname.clone(), tinfo.clone());
                } else {
                    self.types
                        .entry(tname.clone())
                        .or_insert_with(|| tinfo.clone());
                }
            }
        }
        // A selected type is visible with its structure (LRM 10.4): walking
        // `r.f.g` needs the nested record's layout, and `a(i).f` the element
        // record's, even though only the outer type mark was use'd. Merge the
        // type infos reachable through field and element links from the same
        // package alongside the selected one.
        if let Some(i) = item {
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut queue: Vec<String> = vec![i.to_ascii_lowercase()];
            while let Some(cur) = queue.pop() {
                if !seen.insert(cur.clone()) {
                    continue;
                }
                let Some((_, t)) = p.types.iter().find(|(n, _)| n.eq_ignore_ascii_case(&cur))
                else {
                    continue;
                };
                let linked: Vec<String> = match t {
                    TypeInfo::Record { .. } => p
                        .rec_field_tys
                        .iter()
                        .filter(|(r, _, _)| r.eq_ignore_ascii_case(&cur))
                        .map(|(_, _, ft)| ft.clone())
                        .collect(),
                    TypeInfo::Array {
                        elem_name: Some(en),
                        ..
                    } => vec![en.clone()],
                    _ => Vec::new(),
                };
                for n in linked {
                    let nl = n.to_ascii_lowercase();
                    if !seen.contains(&nl) {
                        if let Some((_, ti)) =
                            p.types.iter().find(|(tn, _)| tn.eq_ignore_ascii_case(&nl))
                        {
                            self.types.entry(nl.clone()).or_insert_with(|| ti.clone());
                            queue.push(nl);
                        }
                    }
                }
            }
        }
        for (cname, cval) in &p.consts {
            if item.is_none_or(|i| i == cname) || lit_ok.as_ref().is_some_and(|l| l.contains(cname))
            {
                if force {
                    Rc::make_mut(&mut self.consts).insert(cname.clone(), cval.clone());
                } else {
                    Rc::make_mut(&mut self.consts)
                        .entry(cname.clone())
                        .or_insert_with(|| cval.clone());
                }
            }
        }
        for f in &p.functions {
            if item.is_none_or(|i| i == f.name.to_ascii_lowercase()) {
                // The package body (with real bodies) follows the
                // declaration (signature-only) in the merged package;
                // register_func replaces the earlier candidate.
                self.register_func(f.clone());
            }
        }
        for a in &p.attr_decls {
            if item.is_none_or(|i| i == a) {
                self.attr_decls.insert(a.clone());
            }
        }
        for (r, f, t) in &p.rec_field_tys {
            self.rec_field_ty
                .entry((r.clone(), f.clone()))
                .or_insert_with(|| t.clone());
        }
        for (r, f, d) in &p.rec_field_dims {
            self.rec_field_dims
                .entry((r.clone(), f.clone()))
                .or_insert_with(|| d.clone());
        }
        for (n, pt) in &p.prot_tys {
            if item.is_none_or(|i| i == n) {
                // Last wins: the package body (state fields + initializers)
                // follows the declaration (methods only).
                self.prot_types.insert(n.clone(), pt.clone());
            }
        }
        for (n, tn) in &p.sig_types {
            self.pkg_sig_types
                .entry(n.clone())
                .or_insert_with(|| tn.clone());
        }
        for (n, w) in &p.sig_widths {
            self.pkg_sig_widths.entry(n.clone()).or_insert(*w);
        }
        for (n, tn) in &p.const_tys {
            if item.is_none_or(|i| i == n) {
                self.const_types
                    .entry(n.clone())
                    .or_insert_with(|| tn.clone());
            }
        }
        for (n, d) in &p.const_dims {
            if item.is_none_or(|i| i == n) {
                self.obj_dims.entry(n.clone()).or_insert_with(|| d.clone());
            }
        }
        for (n, w) in &p.type_widths {
            if item.is_none_or(|i| i == n) {
                self.type_widths.entry(n.clone()).or_insert(*w);
            }
        }
        for (n, w) in &p.type_elem {
            if item.is_none_or(|i| i == n) {
                self.type_elem.entry(n.clone()).or_insert(*w);
            }
        }
        for (n, d) in &p.type_dims {
            if item.is_none_or(|i| i == n) {
                self.type_dims.entry(n.clone()).or_insert_with(|| d.clone());
            }
        }
        // Use clauses are transitive through packages: `use work.p.all` also
        // exposes what `p` itself made visible via `use lib.q.all;`.
        if item.is_none() && visited.insert(p.name.to_ascii_lowercase()) {
            let uses = p.uses.clone();
            for u in uses {
                if let Some(q) = self.pkgs.get(&u).cloned() {
                    self.merge_package_rec(&q, None, false, visited);
                }
            }
        }
    }

    /// Register a `package p is new g generic map (...);` found in a
    /// declarative part: instantiate it, make its items visible in the
    /// current scope (overriding the generic package's defaults), and add it
    /// to the known-package map so a following `use work.p.all;` resolves.
    pub(super) fn register_package_inst(&mut self, d: &SyntaxNode) {
        if let Some(q) = self.lower_package_inst(d) {
            let key = q.name.to_ascii_lowercase();
            self.merge_package(&q, None, true);
            self.pkgs.insert(key.clone(), Rc::new(q.clone()));
            self.inst_packages.push(q);
        }
    }

    pub(super) fn lower_package(
        &mut self,
        p: &SyntaxNode,
        root: &SyntaxNode,
        overrides: &HashMap<String, Value>,
    ) -> PackageIr {
        // Package name: the first ident that is not a keyword — `package NAME
        // is …` and `package body NAME is …` both yield NAME (a naive nth(1)
        // would return "body" for package bodies).
        let name = package_decl_name(p, self.db);
        self.cur_scope = name.to_ascii_lowercase();
        let kids0 = real_children(p);
        // A package body sees the package declaration's items, even when the
        // declaration lives in another file compiled earlier: merge the
        // already-registered declaration into scope first.
        if kids0
            .iter()
            .any(|k| k.kind() == SyntaxKind::Ident && self.kw(k) == "body")
        {
            if let Some(decl) = self.pkgs.get(&name.to_ascii_lowercase()).cloned() {
                self.merge_package(&decl, None, false);
                // A package body also sees its declaration's context clauses
                // (bodies often repeat no `use` clauses of their own).
                if let Some(src) = &decl.src {
                    let src = src.clone();
                    self.apply_context_clauses(&src);
                }
            }
        }
        // Package declarations: constants, types, and function bodies become
        // visible in this file.
        let types_before: std::collections::HashSet<String> = self.types.keys().cloned().collect();
        let consts_before = self.consts.clone();
        // Snapshot so the package body's protected-type entry (which gains
        // state fields + initializers over the declaration's) is exported too.
        let prot_before = self.prot_types.clone();
        // VHDL-2008 generic clause first, so constants/types declared after it
        // see the generic values (`generic map` overrides win over defaults).
        let mut generics: Vec<(String, Value)> = Vec::new();
        for d in real_children(p) {
            if d.kind() == SyntaxKind::GenericClause {
                for (n, v, _ty) in self.lower_generic_decls(&d, overrides) {
                    generics.push((n.to_ascii_lowercase(), v));
                }
            }
        }
        let mut functions: Vec<FuncIr> = Vec::new();
        let mut sig_types: Vec<(String, String)> = Vec::new();
        let mut sig_widths: Vec<(String, usize)> = Vec::new();
        let mut uses: Vec<String> = context_units(self.db, root);
        uses.retain(|u| u != &name.to_ascii_lowercase());
        let mut attr_decls: Vec<String> = Vec::new();
        for d in real_children(p) {
            match d.kind() {
                SyntaxKind::ConstantDecl => self.lower_constant_decl(&d),
                SyntaxKind::TypeDecl => self.lower_type_decl(&d),
                SyntaxKind::SubtypeDecl => self.lower_subtype_decl(&d),
                SyntaxKind::ViewDecl => self.lower_view_decl(&d),
                SyntaxKind::AliasDecl => {
                    self.lower_alias_decl(&d);
                    // `alias runner_phase is runner(...)` where runner is a
                    // package-level signal: export the alias name like the
                    // signal, so using files resolve bare `runner_phase`.
                    let ak = real_children(&d);
                    if let Some(npos) = ak.iter().position(|k| k.kind() == SyntaxKind::Name) {
                        let aname = self.kw(&ak[npos]);
                        if let Some(is_pos) = ak
                            .iter()
                            .position(|k| k.kind() == SyntaxKind::Ident && self.kw(k) == "is")
                        {
                            if let Some(target) =
                                ak[is_pos + 1..].iter().find(|k| is_expr_node(k.kind()))
                            {
                                let ttext = self.text(target);
                                let base = ttext
                                    .split(['.', '('])
                                    .next()
                                    .unwrap_or(ttext.as_str())
                                    .to_ascii_lowercase();
                                if let Some(tn) = self.pkg_sig_types.get(&base).cloned() {
                                    self.pkg_sig_types.insert(aname.clone(), tn.clone());
                                    sig_types.push((aname, tn));
                                }
                            }
                        }
                    }
                }
                SyntaxKind::AttrDecl => {
                    if let Some(a) = self.register_attr_decl(&d) {
                        attr_decls.push(a);
                    }
                }
                SyntaxKind::UseClause => {
                    uses.extend(use_clause_units(&d, self.db));
                    self.merge_use_clause(&d);
                }
                SyntaxKind::PackageInst => self.register_package_inst(&d),
                SyntaxKind::FileDecl => self.lower_file_decl(&d),
                // Package-level signal / shared variable: remember name →
                // type so `name.field` / `name.method(...)` resolve in the
                // package body (not simulated).
                SyntaxKind::SignalDecl | SyntaxKind::VariableDecl => {
                    let dk = real_children(&d);
                    let colon = dk
                        .iter()
                        .position(|k| k.kind() == SyntaxKind::Punct && self.text(k) == ":");
                    if let Some(c) = colon {
                        let names: Vec<String> = dk[..c]
                            .iter()
                            .filter(|k| k.kind() == SyntaxKind::Name)
                            .map(|k| self.kw(k))
                            .collect();
                        let tmark = dk[c + 1..].iter().find(|k| k.kind() == SyntaxKind::Name);
                        if let Some(tm) = tmark {
                            let tn = self
                                .kw(tm)
                                .rsplit('.')
                                .next()
                                .unwrap_or_default()
                                .to_string();
                            let constraint =
                                dk[c + 1..].iter().find(|k| k.kind() == SyntaxKind::Range);
                            // Width is best-effort here (package-level signals are
                            // not simulated): never emit "unknown type" from this
                            // bookkeeping — the declaration itself was checked
                            // when the package body was lowered.
                            self.suppress_err += 1;
                            let (w, _, _, _) = self.resolve_type(tm, constraint, d.span());
                            self.suppress_err -= 1;
                            for n in names {
                                self.pkg_sig_types.insert(n.clone(), tn.clone());
                                sig_types.push((n.clone(), tn.clone()));
                                sig_widths.push((n, w));
                            }
                        }
                    }
                }
                SyntaxKind::Function => {
                    // Functions with parameter-dependent local widths (e.g.
                    // `variable v : unsigned(a'length-1 downto 0);` in
                    // std_logic_arith) lower fine in slot space — the width just
                    // defaults when the constraint cannot be const-evaled. Only
                    // constant *initializers* calling such functions fall back to
                    // X (reported separately at the initializer).
                    if let Some(f) = self.lower_function_node(&d) {
                        self.register_func(f.clone());
                        functions.push(f);
                    }
                }
                _ => {}
            }
        }
        // Protected-type methods + generated ctors lowered inside type bodies.
        functions.append(&mut self.pending_funcs);
        // Collect the items this package added, for cross-file `use` clauses.
        let types: Vec<(String, TypeInfo)> = self
            .types
            .iter()
            .filter(|(k, _)| !types_before.contains(*k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        // Single-character consts are usually char-type enum literals ('U',
        // '0', ...); they stay harmless in the package because name resolution
        // checks signals before constants.
        // A package's own declarations must be exported even when a `use`d
        // package already brought a same-named const into scope (e.g. an enum
        // label like ENABLED vs OsvvmOptionsType's) — keep new or overridden
        // entries.
        let consts: Vec<(String, Value)> = self
            .consts
            .iter()
            .filter(|(k, v)| consts_before.get(*k) != Some(*v))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let type_names: std::collections::HashSet<&String> = types.iter().map(|(k, _)| k).collect();
        let rec_field_tys = self
            .rec_field_ty
            .iter()
            .filter(|((r, _), _)| type_names.contains(r))
            .map(|((r, f), t)| (r.clone(), f.clone(), t.clone()))
            .collect();
        let rec_field_dims = self
            .rec_field_dims
            .iter()
            .filter(|((r, _), _)| type_names.contains(r))
            .map(|((r, f), d)| (r.clone(), f.clone(), d.clone()))
            .collect();
        let prot_tys = self
            .prot_types
            .iter()
            .filter(|(n, pt)| {
                type_names.contains(n)
                    || prot_before.get(*n).is_none_or(|old| {
                        old.fields != pt.fields
                            || old.methods != pt.methods
                            || old.inits.len() != pt.inits.len()
                    })
            })
            .map(|(n, pt)| (n.clone(), pt.clone()))
            .collect();
        let const_names: std::collections::HashSet<&String> =
            consts.iter().map(|(k, _)| k).collect();
        let const_tys = self
            .const_types
            .iter()
            .filter(|(k, _)| const_names.contains(k))
            .map(|(k, t)| (k.clone(), t.clone()))
            .collect();
        let const_dims = self
            .obj_dims
            .iter()
            .filter(|(k, _)| const_names.contains(k))
            .map(|(k, d)| (k.clone(), d.clone()))
            .collect();
        // Subtype widths: a vector subtype declared here must keep its width
        // when another file `use`s this package (`resolve_type` consults the
        // width map for Vector types; per-file maps don't cross files).
        let type_widths = types
            .iter()
            .filter(|(n, t)| {
                matches!(t, TypeInfo::Vector { .. }) && self.type_widths.contains_key(n.as_str())
            })
            .filter_map(|(n, _)| self.type_widths.get(n.as_str()).map(|w| (n.clone(), *w)))
            .collect();
        // Same shape rules for the element width (`string` subtypes are
        // character arrays): value-shape folds must divide by 8, not 1,
        // in files that `use` this package.
        let type_elem = types
            .iter()
            .filter(|(_, t)| matches!(t, TypeInfo::Vector { .. }))
            .filter_map(|(n, _)| self.type_elem.get(n.as_str()).map(|e| (n.clone(), *e)))
            .collect();
        // Same for the subtype's constrained range (aggregate layout and
        // `sig'range` need the low bound, not just the width).
        let type_dims = types
            .iter()
            .filter(|(_, t)| matches!(t, TypeInfo::Vector { .. }))
            .filter_map(|(n, _)| {
                self.type_dims
                    .get(n.as_str())
                    .cloned()
                    .map(|d| (n.clone(), d))
            })
            .collect();
        PackageIr {
            name,
            types,
            consts,
            functions,
            generics,
            type_actuals: Vec::new(),
            rec_field_tys,
            rec_field_dims,
            prot_tys,
            sig_types,
            sig_widths,
            const_tys,
            type_widths,
            type_dims,
            type_elem,
            const_dims,
            decl: Some(p.clone()),
            src: Some(root.clone()),
            uses,
            attr_decls,
        }
    }

    /// Lower a VHDL-2008 package instantiation
    /// (`package p is new g generic map (X => v);`): re-lower the generic
    /// package `g` in a fresh scope with the `generic map` values overriding
    /// the generic defaults, and register the result under the new name `p`.
    pub(super) fn lower_package_inst(&mut self, node: &SyntaxNode) -> Option<PackageIr> {
        let kids = real_children(node);
        let inst_name = kids
            .iter()
            .filter(|k| k.kind() == SyntaxKind::Ident)
            .map(|k| self.kw(k))
            .find(|w| !matches!(w.as_str(), "package" | "is" | "new" | "generic" | "map"))
            .unwrap_or_default();
        let target = kids.iter().find(|k| k.kind() == SyntaxKind::Name)?;
        let target_text = self.text(target);
        let db = self.db;
        let unit = target_text
            .rsplit('.')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        let Some(tp) = self.pkgs.get(&unit).cloned() else {
            let candidates: Vec<String> = self.pkgs.keys().cloned().collect();
            self.diags.push(Diagnostic {
                code: "E0401",
                severity: Severity::Error,
                span: Some(target.span()),
                message: format!("unknown package '{unit}' in package instantiation"),
                help: did_you_mean(&unit, candidates.iter())
                    .map(|s| format!("did you mean '{s}'?")),
            });
            return None;
        };
        // Without the generic package's source we can't re-lower it; the
        // already-lowered items are still correct, so alias them under the
        // new name.
        let Some(src_root) = tp.src.clone() else {
            let mut p = (*tp).clone();
            p.name = inst_name;
            return Some(p);
        };
        // All declaration units of the generic package in its source file
        // (declaration carries the generic clause, body the subprograms).
        let decl_nodes: Vec<SyntaxNode> = real_children(&src_root)
            .into_iter()
            .filter(|c| c.kind() == SyntaxKind::Package && package_decl_name(c, db) == unit)
            .collect();
        if decl_nodes.is_empty() {
            let mut p = (*tp).clone();
            p.name = inst_name;
            return Some(p);
        }
        // Fresh scope: builtins + this file's and the generic package's own
        // context clauses + all known packages.
        let file_root = self.file_root.clone();
        let mut lx2 = Lower::new(self.db, self.diags);
        lx2.register_builtin_types();
        lx2.pkgs = self.pkgs.clone();
        lx2.funcs_ce = self.funcs_ce.clone();
        // The enclosing scope (entity generics, constants, types) is visible in
        // the `generic map` associations — but seed it WITHOUT the generic
        // package's own exports: pass-0 merges each instantiation into the
        // enclosing scope with force, so a second `package p2 is new g ...` in
        // the same file would otherwise see g's items as pre-existing and
        // export an empty delta.
        let tp_t: std::collections::HashSet<&String> = tp.types.iter().map(|(k, _)| k).collect();
        let tp_c: std::collections::HashSet<&String> = tp.consts.iter().map(|(k, _)| k).collect();
        lx2.consts = Rc::new(
            self.consts
                .iter()
                .filter(|(k, _)| !tp_c.contains(k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        );
        lx2.types = self
            .types
            .iter()
            .filter(|(k, _)| !tp_t.contains(k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if let Some(r) = &file_root {
            lx2.apply_context_clauses(r);
        }
        lx2.apply_context_clauses(&src_root);
        // `generic map` associations: PortConn children with a leading Name.
        let mut overrides: HashMap<String, Value> = HashMap::new();
        let mut type_actuals: Vec<(String, String)> = Vec::new();
        for k in real_children(node) {
            if k.kind() != SyntaxKind::PortConn {
                continue;
            }
            let ck = real_children(&k);
            // `assoc_formal` logic inlined (self is borrowed by `lx2`); the
            // arrow is the first adjacent `=` `>` punct pair.
            let name_pos = ck
                .windows(2)
                .position(|w| {
                    w[0].kind() == SyntaxKind::Punct
                        && db.span_text(w[0].span()).trim() == "="
                        && w[1].kind() == SyntaxKind::Punct
                        && db.span_text(w[1].span()).trim() == ">"
                })
                .and_then(|arrow| {
                    ck[..arrow]
                        .iter()
                        .rposition(|c| c.kind() == SyntaxKind::Name)
                });
            let assoc = name_pos.map(|i| db.span_text(ck[i].span()).trim().to_ascii_lowercase());
            let expr = ck
                .iter()
                .enumerate()
                .rev()
                .find(|(i, c)| Some(*i) != name_pos && is_expr_node(c.kind()))
                .map(|(_, c)| c);
            if let (Some(n), Some(e)) = (assoc, expr) {
                let ex = lx2.lower_expr(e, &lx2.consts.clone());
                match lx2.ce_env(&ex, &HashMap::new()) {
                    Some(v) => {
                        overrides.insert(n, v);
                    }
                    // Not a constant: a generic `type` parameter bound to an
                    // actual type name (`generic map (type T => u)`).
                    None if matches!(e.kind(), SyntaxKind::Name) => {
                        let actual = db
                            .span_text(e.span())
                            .trim()
                            .rsplit('.')
                            .next()
                            .unwrap_or("")
                            .to_ascii_lowercase();
                        if !actual.is_empty() {
                            type_actuals.push((n, actual));
                        }
                    }
                    None => lx2.warn(
                        e.span(),
                        format!(
                            "generic '{n}' has a non-constant value in the package instantiation; keeping its default"
                        ),
                    ),
                }
            }
        }
        let mut pkg = PackageIr {
            name: inst_name,
            ..Default::default()
        };
        for dn in &decl_nodes {
            let part = lx2.lower_package(dn, &src_root, &overrides);
            pkg.types.extend(part.types);
            pkg.consts.extend(part.consts);
            pkg.functions.extend(part.functions);
            pkg.uses.extend(part.uses);
            pkg.attr_decls.extend(part.attr_decls);
            pkg.rec_field_tys.extend(part.rec_field_tys);
            pkg.rec_field_dims.extend(part.rec_field_dims);
            pkg.prot_tys.extend(part.prot_tys);
            pkg.sig_types.extend(part.sig_types);
            pkg.type_widths.extend(part.type_widths);
            pkg.type_dims.extend(part.type_dims);
            pkg.type_elem.extend(part.type_elem);
            pkg.sig_widths.extend(part.sig_widths);
            pkg.const_tys.extend(part.const_tys);
            pkg.const_dims.extend(part.const_dims);
            if pkg.generics.is_empty() {
                pkg.generics = part.generics;
            }
        }
        // Bind generic type parameters to their actual types in the instance's
        // exported types, so `use work.p.all` exposes the real type (and package
        // bodies of the instance see it).
        for (formal, actual) in &type_actuals {
            if let Some(ti) = lx2.types.get(actual).cloned() {
                match pkg.types.iter_mut().find(|(k, _)| k == formal) {
                    Some(e) => e.1 = ti,
                    None => pkg.types.push((formal.clone(), ti)),
                }
            }
        }
        pkg.type_actuals = type_actuals;
        // The instance is fully bound (no longer generic); a further
        // instantiation of THIS package re-lowers the same generic source
        // with its own overrides (decl/src chain below).
        pkg.generics = Vec::new();
        pkg.decl = tp.decl.clone();
        pkg.src = tp.src.clone();
        Some(pkg)
    }
}
