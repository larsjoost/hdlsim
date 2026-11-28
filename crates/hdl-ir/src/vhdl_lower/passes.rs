//! The per-file lowering passes of `lower_file`, in dependency order: pass 0
//! packages and package instantiations (VHDL-2008 context declarations model
//! as item-less packages), pass 1 entities, configuration declarations, pass
//! 2 architectures (deferring the ones whose entity is not declared in the
//! same file).
//!
//! Extracted from `lower_file` in vhdl_lower.rs — the pass loops moved
//! verbatim; pass 0's match arms became one helper per design-unit kind.

use super::*;

/// Pass 0: packages and package instantiations, before anything that may
/// `use` them — so `use work.my_pkg.all;` also resolves when `my_pkg` is
/// declared in this same file. Each unit registers in `lx.pkgs` (first
/// declaration wins; decl+body merge happens on the design).
pub(super) fn lower_pass0_packages(lx: &mut Lower, root: &SyntaxNode) -> Vec<PackageIr> {
    let mut packages: Vec<PackageIr> = Vec::new();
    for child in real_children(root) {
        match child.kind() {
            SyntaxKind::Package => packages.push(lower_pass0_package(lx, &child, root)),
            SyntaxKind::PackageInst => {
                if let Some(p) = lower_pass0_package_inst(lx, &child) {
                    packages.push(p);
                }
            }
            SyntaxKind::ContextDecl => {
                if let Some(p) = context_decl_package(lx, &child, root) {
                    packages.push(p);
                }
            }
            _ => {}
        }
    }
    packages
}

/// A same-file package declaration, registered in `lx.pkgs` — first
/// (declaration) wins; decl+body merge happens on the design, like for
/// cross-file packages.
fn lower_pass0_package(lx: &mut Lower, p: &SyntaxNode, root: &SyntaxNode) -> PackageIr {
    let p = lx.lower_package(p, root, &HashMap::new());
    lx.pkgs
        .entry(p.name.to_ascii_lowercase())
        .or_insert_with(|| Rc::new(p.clone()));
    p
}

/// A same-file package instantiation, registered over the generic package's
/// defaults that pass 0 put into scope.
fn lower_pass0_package_inst(lx: &mut Lower, inst: &SyntaxNode) -> Option<PackageIr> {
    let p = lx.lower_package_inst(inst)?;
    lx.merge_package(&p, None, true);
    lx.pkgs
        .insert(p.name.to_ascii_lowercase(), Rc::new(p.clone()));
    Some(p)
}

/// A VHDL-2008 context declaration (`context c is use ...; end context;`)
/// exports no items itself, but `context lib.c;` must apply its use/context
/// clauses. Model it as an item-less package whose `uses` are the
/// declaration's clause units, so the existing transitive-`use` merge
/// applies it (and nested context refs).
fn context_decl_package(lx: &mut Lower, decl: &SyntaxNode, root: &SyntaxNode) -> Option<PackageIr> {
    let name = real_children(decl)
        .iter()
        .filter(|k| k.kind() == SyntaxKind::Ident)
        .map(|k| lx.kw(k))
        .find(|w| w != "context")
        .unwrap_or_default();
    if name.is_empty() {
        return None;
    }
    let uses = context_units(lx.db, decl);
    let p = PackageIr {
        name: name.clone(),
        types: Vec::new(),
        consts: Vec::new(),
        functions: Vec::new(),
        generics: Vec::new(),
        type_actuals: Vec::new(),
        rec_field_tys: Vec::new(),
        rec_field_dims: Vec::new(),
        prot_tys: Vec::new(),
        sig_types: Vec::new(),
        sig_widths: Vec::new(),
        const_tys: Vec::new(),
        const_dims: Vec::new(),
        type_widths: Vec::new(),
        type_dims: Vec::new(),
        type_elem: Vec::new(),
        decl: Some(decl.clone()),
        src: Some(root.clone()),
        uses,
        attr_decls: Vec::new(),
    };
    lx.pkgs.entry(name).or_insert_with(|| Rc::new(p.clone()));
    Some(p)
}

/// Pass 1: entities (interfaces) — one module per entity; returns the
/// entity name → file-root list and the entity name → module-index map the
/// later passes and `apply_default_archs` build on.
pub(super) fn lower_pass1_entities(
    lx: &mut Lower,
    root: &SyntaxNode,
    modules: &mut Vec<ModuleIr>,
) -> (Vec<(String, SyntaxNode)>, HashMap<String, usize>) {
    let mut entities: HashMap<String, usize> = HashMap::new();
    let mut entity_roots: Vec<(String, SyntaxNode)> = Vec::new();
    for child in real_children(root) {
        if child.kind() == SyntaxKind::Entity {
            let m = lx.lower_entity(&child);
            entity_roots.push((m.name.clone(), root.clone()));
            entities.insert(m.name.clone(), modules.len());
            modules.push(m);
        }
    }
    (entity_roots, entities)
}

/// Configuration declarations, lowered before the architectures so
/// `configuration work.cfg` instantiations and `use configuration`
/// bindings resolve regardless of declaration order. Returns the
/// configurations and the entity → default-architecture map.
pub(super) fn lower_config_decls(
    lx: &mut Lower,
    root: &SyntaxNode,
) -> (Vec<ConfigIr>, HashMap<String, String>) {
    let mut configurations: Vec<ConfigIr> = Vec::new();
    let mut config_defaults: HashMap<String, String> = HashMap::new();
    for child in real_children(root) {
        if child.kind() == SyntaxKind::ConfigDecl {
            let cfg = lx.lower_config_decl(&child);
            if let Some(arch) = &cfg.arch {
                config_defaults.insert(cfg.entity.clone(), arch.clone());
            }
            lx.cfgs
                .insert(cfg.name.to_ascii_lowercase(), Rc::new(cfg.clone()));
            configurations.push(cfg);
        }
    }
    (configurations, config_defaults)
}

/// Pass 2: architectures (bodies). An architecture whose entity is not
/// declared in this file is deferred (`pending`): the entity may live in a
/// file compiled later, so binding happens design-wide in
/// `lower_pending_architectures`. Returns entity → last architecture name
/// (the implicit default binding).
pub(super) fn lower_pass2_architectures(
    lx: &mut Lower,
    db: &SourceDb,
    root: &SyntaxNode,
    entities: &HashMap<String, usize>,
    modules: &mut Vec<ModuleIr>,
    pending: &mut Vec<PendingArch>,
) -> HashMap<String, String> {
    let mut last_arch: HashMap<String, String> = HashMap::new();
    for child in real_children(root) {
        if child.kind() == SyntaxKind::Architecture {
            let known = arch_entity(&child, db)
                .map(|(_, name)| entities.contains_key(&name))
                .unwrap_or(false);
            if known {
                if let Some((ent, arch)) = lx.lower_architecture(&child, modules, entities) {
                    last_arch.insert(ent, arch);
                }
            } else {
                pending.push(PendingArch {
                    root: root.clone(),
                    arch: child.clone(),
                    lib: None,
                });
            }
        }
    }
    last_arch
}
