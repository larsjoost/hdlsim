//! Elab-facing architecture selection: default-architecture application, pending-architecture lowering, entity-variant resolution, and unit-name stemming. These are the entry points hdl-lib calls after lower_file.
//!
//! Extracted verbatim from vhdl_lower.rs (pure move, no logic changes).

use super::*;

/// Give each plain entity module the body of its default architecture:
/// the one named by a configuration declaration if any, else the last one
/// analyzed (IEEE default binding rule). Bodies live in `entity/arch`
/// variant modules produced by `lower_architecture`.
pub(super) fn apply_default_archs(
    modules: &mut [ModuleIr],
    entities: &HashMap<String, usize>,
    last_arch: &HashMap<String, String>,
    config_defaults: &HashMap<String, String>,
    // Entity slot each entity's default arch was built from. `entities` is
    // name-keyed last-wins, so with same-named entities in different
    // libraries/directories it points at the WRONG slot — the arch module
    // would overwrite a foreign entity and later archs would clone its
    // ports. Callers that know the provenance pass the exact slots here.
    arch_bases: Option<&HashMap<String, usize>>,
) {
    for (ent, &base_from_entities) in entities {
        // Entity and architecture files may spell the entity name with
        // different letter case (VHDL names are case-insensitive: an
        // `tb_X_entity.vhd` may pair with a `tb_x_behavior.vhd`), so match
        // the default-arch records either way.
        let ent_lc = ent.to_ascii_lowercase();
        let ch = match config_defaults
            .iter()
            .find(|(k, _)| k.to_ascii_lowercase() == ent_lc)
            .map(|(_, v)| v.clone())
            .or_else(|| {
                last_arch
                    .iter()
                    .find(|(k, _)| k.to_ascii_lowercase() == ent_lc)
                    .map(|(_, v)| v.clone())
            }) {
            Some(ch) => ch,
            None => continue,
        };
        let vname = format!("{ent}/{ch}");
        if let Some(vpos) = modules
            .iter()
            .position(|m| m.name.eq_ignore_ascii_case(&vname))
        {
            let mut v = modules[vpos].clone();
            v.name = ent.clone();
            let base = arch_bases
                .and_then(|b| {
                    b.iter()
                        .find(|(k, _)| k.to_ascii_lowercase() == ent_lc)
                        .map(|(_, &v)| v)
                })
                .unwrap_or(base_from_entities);
            modules[base] = v;
        }
    }
}

/// Re-lower architectures deferred by [`lower_file`] because their entity was
/// not known when their file was compiled. `modules` is the design-wide module
/// list, so cross-file entity/architecture binding works regardless of file
/// order. Returns the architectures still unresolved; with `emit_errors`,
/// those produce E0401 (caret on the entity name, did-you-mean help over all
/// known entity names).
/// Lowercased file stem with a trailing architecture/entity-role suffix
/// (`-b`, `_rtl`, `-behavior`, ...) removed, so `foo.vhd`'s entity matches
/// `foo_rtl.vhd`'s architecture — the sibling-file convention for designs
/// that split entities and architectures across files. Longest suffix wins,
/// and only one suffix is stripped.
pub fn stem_key(path: &str) -> String {
    let stem = std::path::Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    const SUFFIXES: [&str; 18] = [
        "-behavior",
        "_behavior",
        "-behaviour",
        "_behaviour",
        "-rtl",
        "_rtl",
        "-arch",
        "_arch",
        "-struct",
        "_struct",
        "-entity",
        "_entity",
        "-ent",
        "_ent",
        "-b",
        "_b",
        "-a",
        "_a",
    ];
    for suf in SUFFIXES {
        if let Some(base) = stem.strip_suffix(suf) {
            if !base.is_empty() {
                return base.to_string();
            }
        }
    }
    stem
}

#[allow(clippy::too_many_arguments)]
pub fn lower_pending_architectures(
    db: &SourceDb,
    diags: &mut Vec<Diagnostic>,
    pkgs: &HashMap<String, Rc<PackageIr>>,
    pkg_funcs: &std::rc::Rc<HashMap<String, Vec<std::rc::Rc<FuncIr>>>>,
    cfgs: &HashMap<String, Rc<ConfigIr>>,
    modules: &mut Vec<ModuleIr>,
    pending: Vec<PendingArch>,
    emit_errors: bool,
    // Bind cross-file architectures as soon as their entity is visible.
    // `false` while a compile batch is still running: the provenance maps
    // are incomplete (later files may declare another same-named entity),
    // so binding would latch onto a wrong twin — defer to the final pass.
    bind_eager: bool,
    entity_roots: &HashMap<String, SyntaxNode>,
    entity_maps: &HashMap<String, SigMaps>,
    entity_maps_by_dir: &HashMap<(String, String), SigMaps>,
    entity_by_lib: &HashMap<(String, String), usize>,
    entity_by_dir: &HashMap<(String, String), usize>,
    // (canonical dir, stem key, entity name) → module index — the
    // sibling-file convention (`foo.vhd` entity ↔ `foo_rtl.vhd` arch).
    // Tried before the dir/library maps: a whole-tree library holds several
    // same-named entities, and stem matching is the only signal that pins
    // an architecture file to ITS entity.
    entity_by_stem: &HashMap<(String, String, String), usize>,
    // Out: (module index, architecture source file) for every module this
    // call appended, so callers can stamp source provenance — variant
    // re-lowering and wave/origin attribution must find the file the user
    // actually compiled, not a same-named twin from a library scan.
    bound_sources: &mut Vec<(usize, std::path::PathBuf)>,
) -> Vec<PendingArch> {
    if pending.is_empty() {
        return Vec::new();
    }
    let entities: HashMap<String, usize> = modules
        .iter()
        .enumerate()
        .map(|(i, m)| (m.name.clone(), i))
        .collect();
    let candidates: Vec<String> = entities.keys().cloned().collect();
    let mut still_pending = Vec::new();
    let mut last_arch: HashMap<String, String> = HashMap::new();
    // Entity slot each bound arch cloned from — the slot its default-arch
    // contents may overwrite (same-named entities must not clobber each
    // other, see apply_default_archs).
    let mut arch_bases: HashMap<String, usize> = HashMap::new();
    for p in pending {
        let mut lx = Lower::new(db, diags);
        lx.register_builtin_types();
        lx.file_root = Some(p.root.clone());
        lx.entity_maps = entity_maps.clone();
        lx.entity_maps_by_dir = entity_maps_by_dir.clone();
        lx.pkgs = pkgs
            .iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), v.clone()))
            .collect();
        lx.funcs_ce = pkg_funcs.clone();
        lx.cfgs = cfgs
            .iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), v.clone()))
            .collect();
        // An architecture sees its entity's context clauses even when the
        // entity lives in another file (LRM 1076 §13.3).
        if let Some((_, ename)) = arch_entity(&p.arch, db) {
            if let Some(er) = entity_roots.get(&ename) {
                lx.apply_context_clauses(er);
            }
        }
        lx.apply_context_clauses(&p.root);
        // Bind to the entity from the arch's own source directory first
        // (whole-tree libraries like `work = .` hold same-named entities from
        // several directories; sibling files belong together), then to the
        // entity compiled for the arch's own library (LRM 11.2). The
        // design-wide name lookup applies only when the resolver had no
        // provenance for this file. All of this waits for the final pass
        // while a compile batch is still running (`!bind_eager`): the
        // provenance maps only cover the files compiled so far, and binding
        // to a same-named entity compiled earlier in the batch would latch
        // onto the wrong twin.
        let ents: HashMap<String, usize> = if !bind_eager {
            HashMap::new()
        } else {
            match arch_entity(&p.arch, db) {
                Some((_, en)) => {
                    let key = en.to_ascii_lowercase();
                    let arch_dir: Option<String> = file_dir(db, &p.root);
                    let arch_file = db.file(p.root.span().file).name().to_string();
                    // 1. Sibling-file convention: the arch file's stem with
                    //    the role suffix stripped matches the entity file's
                    //    stem (`reg-b.vhd` belongs to `reg.vhd`, even when
                    //    several entities named `reg` share the directory).
                    let stem_idx = arch_dir.as_ref().and_then(|d| {
                        entity_by_stem.get(&(d.clone(), stem_key(&arch_file), key.clone()))
                    });
                    // 2. Same directory, last entity compiled; 3. same library.
                    let dir_idx = arch_dir
                        .as_ref()
                        .and_then(|d| entity_by_dir.get(&(d.clone(), key.clone())));
                    let lib_idx = p.lib.as_ref().and_then(|lib| {
                        entity_by_lib.get(&(lib.to_ascii_lowercase(), key.clone()))
                    });
                    match stem_idx.or(dir_idx).or(lib_idx) {
                        Some(&i) => [(en, i)].into_iter().collect(),
                        None if p.lib.is_none() && arch_dir.is_none() => match entities.get(&en) {
                            Some(&i) => [(en, i)].into_iter().collect(),
                            None => HashMap::new(),
                        },
                        // Provenance lookup failed. Bind design-wide by name
                        // only when the name is unambiguous — exactly one such
                        // entity was compiled (possibly in another directory).
                        // With several same-named entities and no scope match,
                        // stay pending: a later file may still provide the
                        // right one.
                        None => {
                            let matches: Vec<usize> = modules
                                .iter()
                                .enumerate()
                                .filter(|(_, m)| m.name.eq_ignore_ascii_case(&en))
                                .map(|(i, _)| i)
                                .collect();
                            if matches.len() == 1 {
                                [(en.clone(), matches[0])].into_iter().collect()
                            } else {
                                HashMap::new()
                            }
                        }
                    }
                }
                None => HashMap::new(),
            }
        };
        let first_new = modules.len();
        match lx.lower_architecture(&p.arch, modules, &ents) {
            Some((ent, arch)) => {
                let arch_file = db.file(p.root.span().file).name().to_string();
                for i in first_new..modules.len() {
                    bound_sources.push((i, std::path::PathBuf::from(&arch_file)));
                }
                last_arch.insert(ent.clone(), arch);
                if let Some(&base) = ents.get(&ent) {
                    arch_bases.insert(ent, base);
                }
            }
            None => {
                if emit_errors {
                    lx.err_unknown_entity(&p.arch, &candidates);
                } else {
                    still_pending.push(p);
                }
            }
        }
    }
    apply_default_archs(
        modules,
        &entities,
        &last_arch,
        &HashMap::new(),
        Some(&arch_bases),
    );
    still_pending
}

/// Re-lower one entity and its architectures with `generic map` overrides
/// applied to the entity's generic clause, producing variant modules named
/// `<entity><suffix>` and `<entity><suffix>/<arch>`. Architectures may live
/// in sibling files (`arch_roots`, not including `root`); each gets a fresh
/// scope seeded with the entity's context clauses plus its own file's (LRM
/// 1076 §13.3). Used at elaboration so an instantiation's `generic map`
/// actually re-binds the child's generics (generate ranges, port widths,
/// ...). Returns the lowered modules (empty when `root` does not declare
/// `entity_name`).
#[allow(clippy::too_many_arguments)]
pub fn lower_entity_variant(
    db: &SourceDb,
    diags: &mut Vec<Diagnostic>,
    pkgs: &HashMap<String, Rc<PackageIr>>,
    pkg_funcs: &std::rc::Rc<HashMap<String, Vec<std::rc::Rc<FuncIr>>>>,
    cfgs: &HashMap<String, Rc<ConfigIr>>,
    root: &SyntaxNode,
    arch_roots: &[SyntaxNode],
    entity_name: &str,
    overrides: &HashMap<String, Value>,
    suffix: &str,
) -> Vec<ModuleIr> {
    let mut lx = Lower::new(db, diags);
    lx.register_builtin_types();
    lx.file_root = Some(root.clone());
    lx.pkgs = pkgs
        .iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), v.clone()))
        .collect();
    lx.funcs_ce = pkg_funcs.clone();
    lx.cfgs = cfgs
        .iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), v.clone()))
        .collect();
    lx.apply_context_clauses(root);
    let lname = entity_name.to_ascii_lowercase();
    lx.variant = Some(VariantSpec {
        entity: lname.clone(),
        suffix: suffix.to_string(),
        overrides: overrides.clone(),
    });
    let mut modules: Vec<ModuleIr> = Vec::new();
    let mut entities: HashMap<String, usize> = HashMap::new();
    for child in real_children(root) {
        if child.kind() == SyntaxKind::Entity {
            let ename = real_children(&child)
                .iter()
                .find(|k| {
                    k.kind() == SyntaxKind::Ident
                        && !matches!(lx.kw(k).as_str(), "entity" | "is" | "end" | "begin")
                })
                .map(|k| lx.kw(k))
                .unwrap_or_default();
            if ename.eq_ignore_ascii_case(&lname) {
                entities.insert(lname.clone(), modules.len());
                modules.push(lx.lower_entity(&child));
            }
        }
    }
    if modules.is_empty() {
        return modules;
    }
    let vent = format!("{lname}{suffix}");
    let mut last_arch: HashMap<String, String> = HashMap::new();
    for child in real_children(root) {
        if child.kind() == SyntaxKind::Architecture {
            if let Some((_, en)) = arch_entity(&child, db) {
                if en.eq_ignore_ascii_case(&lname) {
                    if let Some((ent, arch)) =
                        lx.lower_architecture(&child, &mut modules, &entities)
                    {
                        last_arch.insert(ent, arch);
                    }
                }
            }
        }
    }
    // Architectures in sibling files: a fresh scope per file (the entity's
    // context clauses plus the file's own), bound to the variant entity
    // module just lowered.
    let ent_maps = lx.entity_maps.clone();
    let ent_maps_by_dir = lx.entity_maps_by_dir.clone();
    for r in arch_roots {
        let mut lax = Lower::new(db, diags);
        lax.register_builtin_types();
        lax.file_root = Some(r.clone());
        lax.pkgs = pkgs
            .iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), v.clone()))
            .collect();
        lax.funcs_ce = pkg_funcs.clone();
        lax.cfgs = cfgs
            .iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), v.clone()))
            .collect();
        // The entity's signal/type snapshot (record fields, signedness,
        // entity constants and types) — an architecture in another file
        // needs it to resolve field selects (LRM 1076 §1.2).
        lax.entity_maps = ent_maps.clone();
        lax.entity_maps_by_dir = ent_maps_by_dir.clone();
        lax.apply_context_clauses(root);
        lax.apply_context_clauses(r);
        lax.variant = Some(VariantSpec {
            entity: lname.clone(),
            suffix: suffix.to_string(),
            overrides: overrides.clone(),
        });
        let ents: HashMap<String, usize> = [(lname.clone(), 0usize)].into_iter().collect();
        for child in real_children(r) {
            if child.kind() == SyntaxKind::Architecture {
                if let Some((_, en)) = arch_entity(&child, db) {
                    if en.eq_ignore_ascii_case(&lname) {
                        if let Some((ent, arch)) =
                            lax.lower_architecture(&child, &mut modules, &ents)
                        {
                            last_arch.insert(ent, arch);
                        }
                    }
                }
            }
        }
    }
    let mut ventities: HashMap<String, usize> = HashMap::new();
    ventities.insert(vent, 0);
    apply_default_archs(&mut modules, &ventities, &last_arch, &HashMap::new(), None);
    modules
}

/// The entity an architecture belongs to (`architecture NAME of ENTITY is ...`)
/// → (entity ident node, entity name).
pub(super) fn arch_entity(a: &SyntaxNode, db: &SourceDb) -> Option<(SyntaxNode, String)> {
    let kids = real_children(a);
    // idents: [architecture, name, of, entity, is, begin/end...]
    let idents: Vec<&SyntaxNode> = kids
        .iter()
        .filter(|k| k.kind() == SyntaxKind::Ident)
        .collect();
    idents
        .get(3)
        .map(|k| ((*k).clone(), db.span_text(k.span()).trim().to_string()))
}
