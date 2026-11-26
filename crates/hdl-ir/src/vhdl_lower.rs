//! VHDL lowering: CST → the shared IR (`ModuleIr`/`ProcessIr`/`AssignIr`...).
//!
//! One entity + its architectures become one `ModuleIr` (entity = interface:
//! generics/ports; architecture = body: signals, processes, concurrent
//! assignments, instances). Processes lower to `ProcKind::Always` (with a
//! sensitivity list) or `Initial` (wait-driven). VHDL signal assignment `<=`
//! inside a process is non-blocking (delta-cycle) like Verilog NBA; variable
//! assignment `:=` is blocking. The well-known `ieee.std_logic_1164` /
//! `numeric_std` functions (`rising_edge`, `to_integer`, `resize`, …) are
//! built in: recognized by name here and lowered to dedicated `Expr` nodes.

use crate::ir::*;
use crate::levenshtein_bounded;
use crate::value::{BinOp, UnOp, Value, V0, V1, VDASH, VX, VZ};
use crate::verilog_lower::const_eval_f_with_pkgs;
use hdl_syntax::cst::SyntaxNode;
use hdl_syntax::diagnostic::{Diagnostic, Severity};
use hdl_syntax::kinds::SyntaxKind;
use hdl_syntax::source::{FileId, SourceDb, Span};

/// Capacity of a simulated std.textio `line` buffer, in bytes.
pub const TEXTIO_LINE_CAP: usize = 1024;

/// std.textio procedure names that lower to simulated `$textio_*` syscalls
/// (statement position). Their arguments may be plain names (`output`, `l`).
pub(crate) const TEXTIO_PROCS: &[&str] = &[
    "write",
    "read",
    "readline",
    "writeline",
    "endfile",
    "file_open",
    "file_close",
];
use std::collections::HashMap;
use std::rc::Rc;

mod config;
mod dims;
mod dump;
mod entity;
mod expr;
mod objects;
mod types;
mod width;
use variant::{apply_default_archs, arch_entity};
pub use variant::{lower_entity_variant, lower_pending_architectures, stem_key};
mod variant;
pub(crate) use scalar::*;
mod scalar;

/// Lower one parsed VHDL file into modules (entities with their architectures
/// attached). `time_unit_fs` is the fallback for physical literals; VHDL
/// physical literals carry their own unit (`10 ns`), so this is only used for
/// unit-less numbers.
/// What lowering one VHDL file produced: modules plus the package units the
/// file declared (retained on the `Design` for other files' `use` clauses).
pub struct Lowered {
    pub modules: Vec<ModuleIr>,
    pub packages: Vec<PackageIr>,
    /// Configuration declarations of this file, registered design-wide.
    pub configurations: Vec<ConfigIr>,
    /// Architectures whose entity was not declared in the same file; bound
    /// design-wide later via [`lower_pending_architectures`].
    pub pending: Vec<PendingArch>,
    /// Entity name → root of the file that declared the entity. An
    /// architecture inherits its entity's context clauses (LRM 1076 §13.3),
    /// so the pending-architecture pass re-applies them.
    pub entity_roots: Vec<(String, SyntaxNode)>,
    /// Entity name → signal maps + entity-declarative constants/types, so
    /// architectures in other files see the entity's ports and declarative
    /// items (bound design-wide via [`lower_pending_architectures`]).
    pub entity_maps: HashMap<String, SigMaps>,
    /// (source directory, lowercase entity name) → snapshot; disambiguates
    /// same-named entities from different directories (see `Lower`).
    pub entity_maps_by_dir: HashMap<(String, String), SigMaps>,
    /// Constant/generic name → array dimensions [(low, size), …] lowered
    /// from this file (inline constraints, generic clauses, aggregate
    /// defaults). Retained so `--dump-maps` can show what the lowerer
    /// believed when folding `'length(d)`-style attributes.
    pub obj_dims: ObjDims,
}

/// Constant/generic name → array dimensions [(low, size), …] as the
/// lowerer believes them (inline constraints, generic clauses, aggregate
/// defaults). Nameable because it crosses crate boundaries: `Lowered`
/// returns it, `hdl-lib` accumulates it per file, `--dump-maps` prints it.
pub type ObjDims = HashMap<String, Vec<(i64, usize)>>;

/// An architecture deferred until its entity is known design-wide (the entity
/// may be declared in a file compiled later). Holds the file root so the
/// context clauses can be re-applied when the architecture is lowered.
#[derive(Clone, Debug)]
pub struct PendingArch {
    pub root: SyntaxNode,
    pub arch: SyntaxNode,
    /// Library the arch's file was compiled for (stamped by the resolver).
    /// An architecture binds to its own library's entity — another library
    /// may declare an entity with the same name (LRM 11.2).
    pub lib: Option<String>,
}

/// Elaboration variant spec: re-lower `entity` with `overrides` applied to its
/// generic clause; modules are named `<entity><suffix>` / `<entity><suffix>/<arch>`.
struct VariantSpec {
    entity: String,
    suffix: String,
    overrides: HashMap<String, Value>,
}

/// Record field index dims: `(record, field) -> [(low bound, size)]` for
/// fields of multi-bit-element array types. Shared by the lowerer's map and
/// the package export that carries it across files.
type RecFieldDims = Vec<(String, String, Vec<(i64, usize)>)>;

/// A lowered package's declarative items, keyed by unit name on the `Design`,
/// so `use lib.pkg.all;` in another file can merge them into scope.
#[derive(Clone, Debug, Default)]
pub struct PackageIr {
    pub name: String,
    pub types: Vec<(String, TypeInfo)>,
    pub consts: Vec<(String, Value)>,
    /// Function bodies declared in the package — lets constant initializers
    /// const-eval calls like `sel_natural_f(cond, a, b)`.
    pub functions: Vec<FuncIr>,
    /// Generic interface of a VHDL-2008 generic package:
    /// `(name, default value)` per generic, used when the package is
    /// instantiated (`package p is new g generic map (...)`).
    pub generics: Vec<(String, Value)>,
    /// Type actuals of a package instantiation: `(generic type param, actual
    /// type name)` — `package p is new g generic map (type T => u);` binds the
    /// generic type `T` to the actual type `u` inside the instance.
    pub type_actuals: Vec<(String, String)>,
    /// Record field type names `(record, field, field type)` — lets nested
    /// record selects (`r.outer.inner`) resolve in files that `use` the
    /// package (the lowerer's per-file `rec_field_ty` map doesn't cross files).
    pub rec_field_tys: Vec<(String, String, String)>,
    /// Record field index dims `(record, field, [(low, size)])` for fields of
    /// multi-bit-element array types with a decl constraint — lets slices of
    /// such fields (`r.options(0 to n)`) remap in files that `use` the
    /// package, mirroring [`Self::rec_field_tys`].
    pub rec_field_dims: RecFieldDims,
    /// Protected types `(name, layout)` declared by this package — lets
    /// `obj.field` / `obj.m(...)` on a protected object resolve in files that
    /// `use` the package (the lowerer's per-file `prot_types` map doesn't cross
    /// files). The body's entry (with state fields + initializers) wins over
    /// the declaration's.
    pub prot_tys: Vec<(String, ProtType)>,
    /// Package-level signals / shared variables `(name, type mark)` — exported
    /// so `use work.com_pkg.all;` also exposes bare reads of `net` etc.
    pub sig_types: Vec<(String, String)>,
    /// Package-level signal widths `(name, width)` — resolved with the
    /// package's generic values at instantiation, so `pkg.sig'length` is right
    /// for `package p is new g generic map (width => 16)`.
    pub sig_widths: Vec<(String, usize)>,
    /// Typed constants `(name, type mark)` — exported so `pkg_const.field`
    /// (record-typed constant select) resolves in files that `use` the package.
    pub const_tys: Vec<(String, String)>,
    /// Subtype widths `(name, width)` — `resolve_type` falls back to this for
    /// vector subtypes; without it a subtype imported from a package (`use
    /// lib.p.subtype_t;` or `.all`) resolves to width 1. Exported for the same
    /// cross-file reason as `const_tys`.
    pub type_widths: Vec<(String, usize)>,
    /// Vector-subtype constrained ranges `(name, [(low, size), …])` —
    /// crosses files with [`PackageIr::type_widths`].
    pub type_dims: Vec<(String, Vec<(i64, usize)>)>,
    /// Vector-subtype element widths `(name, width)` — `string` subtypes
    /// are character arrays (8), other vector subtypes 1. Crosses files
    /// with [`PackageIr::type_widths`] so a value-shape fold
    /// (`dims_from_init`) divides by the right element width in files
    /// that `use` the package.
    pub type_elem: Vec<(String, usize)>,
    /// Array-typed constant dims `(name, [(low, size), …])` from the
    /// constant's decl constraint — lets `'length(d)`/`'high(d)` on an
    /// imported multidimensional constant fold in files that `use` the
    /// package (per-file `obj_dims` maps don't cross files).
    pub const_dims: Vec<(String, Vec<(i64, usize)>)>,
    /// The package's own declaration node and the design-file root that
    /// declared it — retained so a package instantiation can re-lower the
    /// generic package with the `generic map` values as overrides.
    pub decl: Option<SyntaxNode>,
    pub src: Option<SyntaxNode>,
    /// Unit names this package made visible with its own `use lib.p.all;`
    /// clauses — use clauses are transitive through packages in VHDL, so
    /// `use work.p.all` also exposes what `p` used.
    pub uses: Vec<String>,
    /// User-defined attribute names declared in this package
    /// (`attribute a : T;`), made visible by `use` clauses like other items.
    pub attr_decls: Vec<String>,
}

/// A lowered configuration declaration: `configuration cfg of ent is ... end;`.
#[derive(Clone, Debug, Default)]
pub struct ConfigIr {
    pub name: String,
    /// Bound entity (unit name, lowercased).
    pub entity: String,
    /// Top-level `for arch ... end for;` — the default architecture binding.
    pub arch: Option<String>,
    /// Block configurations (`for label : comp use ...;`, `for all : ...`,
    /// nested `for` blocks flattened into `path`).
    pub blocks: Vec<BlockCfg>,
}

/// One block configuration inside a configuration declaration.
#[derive(Clone, Debug)]
pub struct BlockCfg {
    /// Block specification path: `[arch]`, `[label, comp]`; `all`/`others`
    /// set for the `for all : comp` / `for others : comp` forms.
    pub path: Vec<String>,
    pub all: bool,
    pub others: bool,
    pub binding: Binding,
}

/// One waveform element of a signal assignment: `expr [after t] [when c]`.
struct WaveElem {
    val: SyntaxNode,
    delay: Option<SyntaxNode>,
    cond: Option<SyntaxNode>,
}

/// A binding indication: `use entity lib.e(arch) [maps]` or
/// `use configuration lib.cfg`.
#[derive(Clone, Debug)]
pub enum Binding {
    Entity {
        name: String,
        arch: Option<String>,
        generic_map: Vec<(Option<String>, Expr)>,
        port_map: Vec<Conn>,
    },
    Configuration {
        name: String,
    },
}

/// Library names referenced by a library-wide use clause (`use lib.all;`,
/// `context lib.all;`) at the top of a design file — e.g. `use math.all`
/// yields `math`. The library manager compiles every unit of that library.
pub fn context_libraries(db: &SourceDb, root: &SyntaxNode) -> Vec<String> {
    let mut out = Vec::new();
    // Context declarations (`context c is use ...; end context;`) name their
    // clause units too — descend one level so a context file pulls in what its
    // clauses reference.
    let mut stack: Vec<SyntaxNode> = real_children(root);
    while let Some(child) = stack.pop() {
        if child.kind() == SyntaxKind::ContextDecl {
            stack.extend(real_children(&child));
            continue;
        }
        match child.kind() {
            SyntaxKind::ContextClause | SyntaxKind::UseClause => {
                for n in real_children(&child) {
                    if n.kind() == SyntaxKind::Name {
                        let text = db.span_text(n.span()).trim();
                        let parts: Vec<&str> = text.split('.').collect();
                        if parts.len() == 2 && parts[1].eq_ignore_ascii_case("all") {
                            let lib = parts[0].to_ascii_lowercase();
                            if !lib.is_empty() && !out.contains(&lib) {
                                out.push(lib);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Unit names referenced by the context clauses (`use lib.pkg...;`,
/// `context lib.pkg...;`) at the top of a design file — e.g.
/// `use ieee.std_logic_1164.all` yields `std_logic_1164`. The library manager
/// resolves these before lowering the file so the packages are visible.
pub fn context_units(db: &SourceDb, root: &SyntaxNode) -> Vec<String> {
    let mut out = Vec::new();
    // Context declarations (`context c is use ...; end context;`) name their
    // clause units too — descend so a context file pulls in what its clauses
    // reference.
    let mut stack: Vec<SyntaxNode> = real_children(root);
    while let Some(child) = stack.pop() {
        if child.kind() == SyntaxKind::ContextDecl {
            stack.extend(real_children(&child));
            continue;
        }
        match child.kind() {
            SyntaxKind::ContextClause | SyntaxKind::UseClause => {
                for n in real_children(&child) {
                    if n.kind() == SyntaxKind::Name {
                        let text = db.span_text(n.span()).trim();
                        let parts: Vec<&str> = text.split('.').collect();
                        // `lib.unit.all` / `lib.unit.item` / `unit.all` / `unit.item`
                        // Two parts are ambiguous (`lib.unit` vs `unit.item`):
                        // unless the second part is `all`, treat it as the unit
                        // (`context vunit_lib.com_context;` references the unit
                        // `com_context` in library `vunit_lib`).
                        let unit = match parts.len() {
                            n if n >= 3 => parts[n - 2],
                            2 if parts[1].eq_ignore_ascii_case("all") => parts[0],
                            2 => parts[1],
                            _ => continue,
                        };
                        if !unit.is_empty() && !out.iter().any(|u| u == unit) {
                            out.push(unit.to_ascii_lowercase());
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out.extend(package_inst_target_units(db, root));
    out
}

/// Lowercased names of the design units declared in this file (packages,
/// package instantiations, context declarations, entities, architectures,
/// configurations) — so resolution can skip units the file declares itself
/// (they are lowered by pass 0 of this file, and resolving them externally
/// could pull in a same-named unit from another file).
pub fn declared_unit_names(db: &SourceDb, root: &SyntaxNode) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut stack: Vec<SyntaxNode> = real_children(root);
    while let Some(n) = stack.pop() {
        match n.kind() {
            SyntaxKind::Package => {
                let name = package_decl_name(&n, db).to_ascii_lowercase();
                if !name.is_empty() && !out.contains(&name) {
                    out.push(name);
                }
            }
            SyntaxKind::PackageInst | SyntaxKind::ContextDecl => {
                if let Some(id) = real_children(&n)
                    .iter()
                    .filter(|k| k.kind() == SyntaxKind::Ident)
                    .map(|k| db.span_text(k.span()).trim().to_ascii_lowercase())
                    .find(|w| !matches!(w.as_str(), "package" | "body" | "context" | "is" | "new"))
                {
                    if !out.contains(&id) {
                        out.push(id);
                    }
                }
            }
            _ => stack.extend(real_children(&n)),
        }
    }
    out
}

/// Generic-package targets of all `package p is new g [generic map (...)]`
/// instantiations anywhere in the file (possibly nested in declarative
/// parts) — the first Name child of each PackageInst node (`lib.g` → `g`).
/// The generic map is not descended into: its actuals may name local items.
pub fn package_inst_target_units(db: &SourceDb, root: &SyntaxNode) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut stack: Vec<SyntaxNode> = real_children(root);
    while let Some(n) = stack.pop() {
        if n.kind() == SyntaxKind::PackageInst {
            if let Some(t) = real_children(&n)
                .into_iter()
                .find(|c| c.kind() == SyntaxKind::Name)
            {
                let unit = db
                    .span_text(t.span())
                    .trim()
                    .rsplit('.')
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if !unit.is_empty() && !out.iter().any(|u| u == &unit) {
                    out.push(unit);
                }
            }
        } else {
            stack.extend(real_children(&n));
        }
    }
    out
}

/// `(unit, library)` pairs referenced by the context clauses of a design file —
/// like [`context_units`], but carrying the library qualifier of the `use`
/// clause so the library manager can resolve `use lib.unit` inside `lib`
/// (LRM 11.2) before falling back to the global search. `None` means the
/// library is unknown (package instantiations) and only the global search
/// applies. Library-wide `use lib.all;` clauses are skipped: they carry no
/// unit name and are handled via `context_libraries`.
pub fn context_unit_libs(db: &SourceDb, root: &SyntaxNode) -> Vec<(String, Option<String>)> {
    let mut out: Vec<(String, Option<String>)> = Vec::new();
    // Context declarations (`context c is use ...; end context;`) name their
    // clause units too — descend so a context file pulls in what its clauses
    // reference.
    let mut stack: Vec<SyntaxNode> = real_children(root);
    while let Some(child) = stack.pop() {
        if child.kind() == SyntaxKind::ContextDecl {
            stack.extend(real_children(&child));
            continue;
        }
        match child.kind() {
            SyntaxKind::ContextClause | SyntaxKind::UseClause => {
                for n in real_children(&child) {
                    if n.kind() == SyntaxKind::Name {
                        let text = db.span_text(n.span()).trim();
                        let parts: Vec<&str> = text.split('.').collect();
                        // Same part split as `context_units`: `lib.unit.all` /
                        // `lib.unit.item` name unit `unit` in library `lib`;
                        // bare 2-part names are ambiguous but conventionally
                        // `lib.unit` (see `context_units`).
                        let (unit, lib) = match parts.len() {
                            n if n >= 3 => (parts[n - 2], Some(parts[0])),
                            2 if parts[1].eq_ignore_ascii_case("all") => continue,
                            2 => (parts[1], Some(parts[0])),
                            _ => continue,
                        };
                        if unit.is_empty() {
                            continue;
                        }
                        let lib = lib.map(|l| l.to_ascii_lowercase());
                        let unit = unit.to_ascii_lowercase();
                        if !out.iter().any(|(u, l)| *u == unit && *l == lib) {
                            out.push((unit, lib));
                        }
                    }
                }
            }
            // `package p is new g generic map (...)` depends on `g` and on any
            // package actuals in the map (`generic map (P => work.q)`).
            SyntaxKind::PackageInst => {
                let mut stack: Vec<SyntaxNode> = real_children(&child);
                while let Some(n) = stack.pop() {
                    if n.kind() == SyntaxKind::Name {
                        let unit = db
                            .span_text(n.span())
                            .trim()
                            .rsplit('.')
                            .next()
                            .unwrap_or("")
                            .to_ascii_lowercase();
                        if !unit.is_empty() && !out.iter().any(|(u, _)| *u == unit) {
                            out.push((unit, None));
                        }
                    }
                    stack.extend(real_children(&n));
                }
            }
            _ => {}
        }
    }
    out
}

/// Unit names referenced by a single `use lib.unit...;` clause node — used to
/// record a package's own uses so `use work.p.all` is transitive.
fn use_clause_units(u: &SyntaxNode, db: &SourceDb) -> Vec<String> {
    let mut out = Vec::new();
    for n in real_children(u) {
        if n.kind() == SyntaxKind::Name {
            let text = db.span_text(n.span()).trim();
            let parts: Vec<&str> = text.split('.').collect();
            let unit = match parts.len() {
                n if n >= 3 => parts[n - 2],
                2 => parts[0],
                _ => continue,
            };
            let unit = unit.to_ascii_lowercase();
            if !unit.is_empty() && !out.contains(&unit) {
                out.push(unit);
            }
        }
    }
    out
}

// The lowerer's entry points thread the accumulated design-wide maps
// (packages, configs, entity roots/maps…) from file to file; a params
// struct would just relocate the same fields without hiding any.
#[allow(clippy::too_many_arguments)]
pub fn lower_file(
    file: FileId,
    root: &SyntaxNode,
    db: &SourceDb,
    diags: &mut Vec<Diagnostic>,
    _time_unit_fs: u64,
    pkgs: &HashMap<String, Rc<PackageIr>>,
    pkg_funcs: &std::rc::Rc<HashMap<String, Vec<std::rc::Rc<FuncIr>>>>,
    cfgs: &HashMap<String, Rc<ConfigIr>>,
) -> Lowered {
    let _ = file;
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
    let mut modules: Vec<ModuleIr> = Vec::new();
    let mut entities: HashMap<String, usize> = HashMap::new();
    let mut packages: Vec<PackageIr> = Vec::new();
    let mut configurations: Vec<ConfigIr> = Vec::new();
    let mut pending: Vec<PendingArch> = Vec::new();
    // Pass 0: packages and package instantiations, before anything that may
    // `use` them — so `use work.my_pkg.all;` also resolves when `my_pkg` is
    // declared in this same file.
    for child in real_children(root) {
        match child.kind() {
            SyntaxKind::Package => {
                let p = lx.lower_package(&child, root, &HashMap::new());
                // First (declaration) wins in `pkgs`; decl+body merge happens
                // on the design, like for cross-file packages.
                lx.pkgs
                    .entry(p.name.to_ascii_lowercase())
                    .or_insert_with(|| Rc::new(p.clone()));
                packages.push(p);
            }
            SyntaxKind::PackageInst => {
                if let Some(p) = lx.lower_package_inst(&child) {
                    // The instantiation's items win over the generic
                    // package's defaults that pass 0 put into scope.
                    lx.merge_package(&p, None, true);
                    lx.pkgs
                        .insert(p.name.to_ascii_lowercase(), Rc::new(p.clone()));
                    packages.push(p);
                }
            }
            // A VHDL-2008 context declaration (`context c is use ...; end
            // context;`) exports no items itself, but `context lib.c;` must
            // apply its use/context clauses. Model it as an item-less package
            // whose `uses` are the declaration's clause units, so the existing
            // transitive-`use` merge applies it (and nested context refs).
            SyntaxKind::ContextDecl => {
                let name = real_children(&child)
                    .iter()
                    .filter(|k| k.kind() == SyntaxKind::Ident)
                    .map(|k| lx.kw(k))
                    .find(|w| w != "context")
                    .unwrap_or_default();
                if !name.is_empty() {
                    let uses = context_units(lx.db, &child);
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
                        decl: Some(child.clone()),
                        src: Some(root.clone()),
                        uses,
                        attr_decls: Vec::new(),
                    };
                    lx.pkgs.entry(name).or_insert_with(|| Rc::new(p.clone()));
                    packages.push(p);
                }
            }
            _ => {}
        }
    }
    // Re-apply the context clauses now that same-file packages (plain and
    // instantiated) are in `pkgs`.
    lx.apply_context_clauses(root);
    // Pass 1: entities (interfaces).
    let mut entity_roots: Vec<(String, SyntaxNode)> = Vec::new();
    for child in real_children(root) {
        if child.kind() == SyntaxKind::Entity {
            let m = lx.lower_entity(&child);
            entity_roots.push((m.name.clone(), root.clone()));
            entities.insert(m.name.clone(), modules.len());
            modules.push(m);
        }
    }
    // Configurations first, so `configuration work.cfg` instantiations and
    // `use configuration` bindings resolve regardless of declaration order.
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
    // Pass 2: architectures (bodies). An architecture whose entity is not
    // declared in this file is deferred (`pending`): the entity may live in a
    // file compiled later, so binding happens design-wide in
    // `lower_pending_architectures`.
    let mut last_arch: HashMap<String, String> = HashMap::new();
    for child in real_children(root) {
        if child.kind() == SyntaxKind::Architecture {
            let known = arch_entity(&child, db)
                .map(|(_, name)| entities.contains_key(&name))
                .unwrap_or(false);
            if known {
                if let Some((ent, arch)) = lx.lower_architecture(&child, &mut modules, &entities) {
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
    apply_default_archs(&mut modules, &entities, &last_arch, &config_defaults, None);
    // Package instantiations from declarative parts (registered on the fly).
    packages.append(&mut lx.inst_packages);
    Lowered {
        modules,
        packages,
        configurations,
        pending,
        entity_roots,
        entity_maps: lx.entity_maps.clone(),
        entity_maps_by_dir: lx.entity_maps_by_dir.clone(),
        obj_dims: lx.obj_dims.clone(),
    }
}

/// Result of lowering a concurrent signal assignment: either a continuous
/// assign or (for delayed waveforms) a one-shot process.
pub(crate) enum ConcOut {
    Assign(AssignIr),
    Proc(ProcessIr),
}

/// Canonical source directory of the file a syntax node came from — the key
/// that groups sibling design units (entity + its architectures) together.
fn file_dir(db: &SourceDb, root: &SyntaxNode) -> Option<String> {
    let name = db.file(root.span().file).name().to_string();
    let parent = std::path::Path::new(&name).parent()?.to_path_buf();
    Some(
        std::fs::canonicalize(&parent)
            .map(|c| c.to_string_lossy().to_string())
            .unwrap_or_else(|_| parent.to_string_lossy().to_string()),
    )
}

/// Package declaration name: the first ident that is not a keyword —
/// `package NAME is …` and `package body NAME is …` both yield NAME (a naive
/// nth(1) would return "body" for package bodies).
fn package_decl_name(p: &SyntaxNode, db: &SourceDb) -> String {
    real_children(p)
        .iter()
        .filter(|k| k.kind() == SyntaxKind::Ident)
        .map(|k| db.span_text(k.span()).trim().to_ascii_lowercase())
        .find(|w| !matches!(w.as_str(), "package" | "body" | "is" | "end"))
        .unwrap_or_default()
}

/// Static type info for a VHDL type mark.
#[derive(Clone, Debug)]
pub enum TypeInfo {
    /// Scalar 1-bit (std_logic, bit, boolean).
    Scalar,
    /// Vector (std_logic_vector/unsigned/signed/bit_vector); the width comes
    /// from the constraint at the declaration site.
    Vector { signed: bool },
    /// 32-bit integer (natural/positive unsigned).
    Integer { signed: bool },
    /// 64-bit IEEE-754 real.
    Real,
    /// 8-bit char.
    Char,
    /// Enumeration: literals in declaration order.
    Enum(Vec<String>),
    /// Array type: element width + total depth (memories) + per-index
    /// dimensions [(low, size), …] for multi-dimensional flat indexing.
    /// `elem_name` is the element type mark (for arrays of records, so
    /// `arr(i).field` can be lowered).
    Array {
        elem: usize,
        depth: usize,
        dims: Vec<(i64, usize)>,
        elem_name: Option<String>,
    },
    /// Record type: fields as (name, width, signed) in declaration order.
    /// Lowered as one flat vector; the first declared field is the MSB.
    Record { fields: Vec<(String, usize, bool)> },
    /// Protected type (VHDL-2008): objects are 64-bit handles into the
    /// simulator's object store (same model as SV classes); state lives in
    /// the object word, methods are `t::m` functions.
    Handle,
}

/// A protected type's layout: state variables as (name, width, lsb, signed)
/// in declaration order (first declared = MSB of the object word), plus the
/// method names declared in the protected header.
#[derive(Clone, Debug, Default)]
pub struct ProtType {
    pub fields: Vec<(String, usize, usize, bool)>,
    pub methods: Vec<String>,
    /// State-variable initializers (field name → lowered expr), written by
    /// the generated `t::new` constructor.
    pub inits: Vec<(String, Expr)>,
}

/// One step of a folded selected-name chain: a record field select (`.f`)
/// or an index group (`(i)` / `(i, j)`). The parser folds `a.b(i).c` into
/// nested Name/Call nodes; `flatten_selected_name` recovers the order.
#[derive(Debug)]
enum ChainSeg {
    Field(String),
    Index(Vec<SyntaxNode>),
}

struct Lower<'a> {
    db: &'a SourceDb,
    diags: &'a mut Vec<Diagnostic>,
    /// Compile-time constants in scope (enum literals, constants, generics).
    consts: Rc<HashMap<String, Value>>,
    /// Constant name -> type mark, so record/array-typed constants can be
    /// materialized as hidden signals for runtime indexing (`c(i).field`).
    const_types: HashMap<String, String>,
    /// User function bodies in scope (package functions via `use`), for
    /// const-evaluating calls in constant initializers.
    /// Lowered functions/procedures by bare name. Overloads (same name,
    /// different signatures) coexist as candidates; `register_func` keeps
    /// the map invariant and const-eval picks by argument fit.
    funcs: HashMap<String, Vec<FuncIr>>,
    /// Packages already bulk-merged (`use lib.pkg.all`) into this scope —
    /// merging is transitive, so without this the same package closure is
    /// re-cloned (types/consts/functions) once per use clause per file.
    merged_all: std::collections::HashSet<String>,
    /// Const-eval fallback: function bodies from every known package,
    /// shared design-wide (`Design::pkg_funcs`) and only consulted when the
    /// direct `self.funcs` map misses. A use clause may merge only one
    /// function of a package, yet its body may legally call a
    /// package-internal sibling (visible at the body's declaration site), so
    /// nested const-eval resolves against this wider table when the direct
    /// map misses. Cross-file packages only: same-file packages register
    /// their functions into `self.funcs` directly.
    funcs_ce: std::rc::Rc<HashMap<String, Vec<std::rc::Rc<FuncIr>>>>,
    /// Type marks in scope (built-ins + user type/subtype decls).
    types: HashMap<String, TypeInfo>,
    /// Subtype name -> constrained width, so `signal x : my_subtype` (no
    /// constraint at the use site) keeps the subtype's width.
    type_widths: HashMap<String, usize>,
    /// Element width per Vector-subtype name (`string` subtypes: 8) —
    /// `array_elem_width` consults it; exported via
    /// [`PackageIr::type_elem`] for cross-file folds.
    type_elem: HashMap<String, usize>,
    /// Subtype name -> constrained range [(low, size), …] for vector
    /// subtypes, so aggregates and `'range`/`'length` see the subtype's
    /// bounds (Array subtypes carry theirs in `TypeInfo::Array.dims`).
    type_dims: HashMap<String, Vec<(i64, usize)>>,
    /// Signal/variable name → signal id in the current module.
    names: HashMap<String, SigId>,
    /// Signal id → memory depth (for `name'range` loops over arrays).
    mems: HashMap<SigId, usize>,
    /// Signal id → signed flag (for `resize`/`to_signed` context).
    sig_signed: HashMap<SigId, bool>,
    /// Signal id → record type name (for `sig.field` selects), if record-typed.
    sig_rec: HashMap<SigId, String>,
    /// (record type, field name) → effective element dims [(low, size), …]
    /// for fields whose type is an unconstrained array with a decl-site
    /// constraint (`leaves : leaf_vec_t(0 to 3)`) — the chain walker needs
    /// the low bound to flatten indexes over the field's slice.
    rec_field_dims: HashMap<(String, String), Vec<(i64, usize)>>,
    /// Package-level signal / shared variable name → type mark, so that
    /// `name.field` (record field) and `name.method(...)` (protected type)
    /// resolve when lowering package bodies.
    pkg_sig_types: HashMap<String, String>,
    /// Package-level signal widths (name → width) from merged packages.
    pkg_sig_widths: HashMap<String, usize>,
    /// Signal id → element record type name for arrays of records, so
    /// `arr(i).field` lowers to a slice of the memory element.
    sig_elem_rec: HashMap<SigId, String>,
    /// Signal id → width in bits (for `'length`/`'high`/… attributes).
    sig_width: HashMap<SigId, usize>,
    /// Signal id → array dimensions [(low, size), …] (for `m(i, j)` flat index).
    sig_dims: HashMap<SigId, Vec<(i64, usize)>>,
    /// Constant/generic name → array dimensions [(low, size), …] from its
    /// decl constraint, default, or aggregate actual — lets `'length(d)` /
    /// `'high(d)` fold on multidimensional constants and generics (the
    /// object constraint is not part of the type mark, so `types` alone
    /// only knows the unconstrained base's dims).
    obj_dims: ObjDims,
    /// Signal id → (low bound, ascending?) of a vector's own index
    /// constraint — ascending ranges (`0 to N-1`) address flat bits
    /// mirrored (element k = value bit W-1-k), so slices must remap.
    sig_asc: HashMap<SigId, (i64, bool)>,
    /// Signal id → element width in bits for multi-bit-element vectors
    /// (`string`: 8-bit characters) — index/slice bounds are element
    /// positions, not bit positions, so addressing must scale by this.
    /// Absent = 1-bit elements (plain vectors).
    sig_elem: HashMap<SigId, usize>,
    /// Signal id → bit-width expression for function locals whose
    /// constraint hangs on a subprogram formal (`string(1 to size)`) —
    /// index/slice remaps must use the per-call width, not the recorded
    /// fallback (`FuncIr.dyn_locals` is the const-eval twin).
    sig_dynw: HashMap<SigId, Expr>,
    /// Total bits of the assignment/initializer target while its aggregate
    /// expression lowers, when the target is a string (8-bit elements): an
    /// `(others => ' ')` replicate must cover the whole target, and without
    /// the target context it baked a fixed 32-bit count. `None` outside
    /// string-target aggregate lowering keeps every other aggregate at its
    /// previous behavior.
    agg_target_w: Option<usize>,
    /// Access type name → designated type name, so `p.field` on an
    /// access-to-record lowers as an implicit `.all` dereference.
    access_inner: HashMap<String, String>,
    /// (record type, field) → field type mark, for nested record selects
    /// (`rv.param.distribution`).
    rec_field_ty: HashMap<(String, String), String>,
    /// User-declared physical type unit name -> value in the type's base
    /// unit (`units angstrom; nm = 10 angstrom; ...` -- `nm` -> 10). Lets
    /// physical literals (`1.5 cm`) and bare unit names (`mm`) lower to a
    /// base-unit integer instead of erroring.
    phys_units: HashMap<String, u64>,
    /// Hidden signals allocated while lowering statements (for-loop vars),
    /// appended to the module after the process body is lowered.
    extra_signals: Vec<SignalIr>,
    /// `module.signals.len()` captured when statement lowering starts.
    extra_base: usize,
    /// Enclosing function scopes while lowering a nested function body, so a
    /// reference to an outer parameter/local (`buf.all` inside a nested
    /// function) resolves instead of erroring.
    names_stack: Vec<HashMap<String, SigId>>,
    /// Counter for auto-naming instances (the parser drops statement labels,
    /// so `InstIr.name` is always empty; elab needs unique prefixes).
    inst_count: usize,
    /// Ports declared without a constraint (`port (a : in std_ulogic_vector)`)
    /// — their width is only known after port connection, so static
    /// attributes on them lower to runtime `Expr::SigAttr`.
    uncon_ports: std::collections::HashSet<SigId>,
    /// Slot ids of the subprogram formals being lowered — their sizes come
    /// from the call's actuals, so `'length`/`'high` must fold symbolically
    /// even when a previously lowered subprogram left a stale `sig_width`/
    /// `sig_dims` entry for the same slot id (slot ids alias across
    /// subprograms).
    fn_formals: std::collections::HashSet<SigId>,
    /// `alias name is <expr>;` — name → lowered expression (slice/index
    /// aliases can't be a plain SigId). Whole-signal aliases are additionally
    /// inserted into `names`.
    aliases: HashMap<String, Expr>,
    /// Record type of a record alias (`alias LAW : R'subtype is s.field;`), so
    /// `LAW.field` selects slice the aliased expression.
    alias_rec: HashMap<String, String>,
    /// >0 while lowering binding-actual expressions in configuration
    /// > specifications: actuals name signals of the enclosing architecture,
    /// > which are not in scope design-wide, so E0401s are suppressed.
    suppress_err: u32,
    /// Depth of function-body lowering. Inside a body, `names` holds exactly
    /// the formals/locals (slot map), so a formal must shadow an outer
    /// constant that reached the lowering env snapshot (an architecture
    /// constant `C_DATA` colliding with a formal `C_DATA` resolves to the
    /// formal — LRM 10.3 inner visibility).
    fn_scope: usize,
    /// All packages visible while lowering this file (library packages from
    /// other files plus same-file ones), keyed by lowercased unit name —
    /// backs `use lib.pkg...;` inside declarative parts and package
    /// instantiations.
    pkgs: HashMap<String, Rc<PackageIr>>,
    /// Configurations visible while lowering this file (design-wide, from
    /// other files plus same-file ones) — backs `configuration work.cfg`
    /// instantiations and `use configuration` binding indications.
    cfgs: HashMap<String, Rc<ConfigIr>>,
    /// The design file currently being lowered (for re-applying its context
    /// clauses when re-lowering a generic package at an instantiation site).
    file_root: Option<SyntaxNode>,
    /// Packages instantiated inside declarative parts (architecture/process/
    /// generate/package bodies) — returned with the file so they register on
    /// the design (their functions are needed at runtime).
    inst_packages: Vec<PackageIr>,
    /// Per-entity snapshot of the signal-id → metadata maps. `SigId` is
    /// module-relative, so with several entities per file the maps would
    /// otherwise leak across entities; the architecture of entity E installs
    /// E's snapshot (see lower_architecture).
    entity_maps: HashMap<String, SigMaps>,
    /// Same snapshots keyed by (source directory, lowercase entity name).
    /// Two libraries (or two directories of a whole-tree library) may declare
    /// same-named entities with different ports; the name-keyed map alone is
    /// first-wins and would install the wrong entity's scope into a pending
    /// architecture. The dir-keyed entry disambiguates siblings.
    entity_maps_by_dir: HashMap<(String, String), SigMaps>,
    /// Delta of `consts`/`types` declared in the entity declarative part,
    /// exported to the entity's architectures via `SigMaps`.
    entity_consts: HashMap<String, Value>,
    entity_types: HashMap<String, TypeInfo>,
    /// Delta of `obj_dims` recorded in the entity declarative part (generics
    /// and entity constants), exported to the entity's architectures via
    /// `SigMaps` — per-file `obj_dims` maps don't cross files, so a generic's
    /// `'length(d)` in the architecture needs this handoff.
    entity_obj_dims: ObjDims,
    /// Delta of `aliases`/`alias_rec` declared in the entity declarative part,
    /// exported to the entity's architectures via `SigMaps` (entity-level
    /// `alias`/`signal` declarations are visible in every architecture of the
    /// entity, LRM 1076 §1.2 — the entity's declarative region encompasses
    /// its architectures).
    entity_aliases: HashMap<String, Expr>,
    entity_alias_rec: HashMap<String, String>,
    /// Set when re-lowering one entity as an elaboration variant (generic-map
    /// overrides); modules get the `<entity><suffix>` name.
    variant: Option<VariantSpec>,
    /// Protected types (VHDL-2008): type name → state fields + methods.
    prot_types: HashMap<String, ProtType>,
    /// Protected type whose body is currently being lowered (bare state-var
    /// and method references resolve against it).
    prot_body_ctx: Option<String>,
    /// Signal id → protected type name (objects holding a 64-bit handle).
    sig_prot: HashMap<SigId, String>,
    /// FuncIrs lowered inside protected bodies / ctor — drained into the
    /// enclosing package's or module's function list.
    pending_funcs: Vec<FuncIr>,
    /// Entity currently being lowered (variant override matching).
    cur_entity: String,
    /// Scope label for `--dump-consts` records: the package name, or the
    /// `entity` / `entity/arch` module currently being lowered.
    cur_scope: String,
    /// String-typed object context: string literals in the initializer of a
    /// `string` (or string-subtype) object are ASCII text, not bit strings —
    /// an all-alphabet literal like `"0x0000"` must lower 8 bits/char, or its
    /// `x` characters X-poison the value and kill `'length` downstream. Set
    /// around initializer lowering only (see `string_typed`).
    str_ctx: bool,
    /// Names currently bound to no value whose X roots in an unbound formal
    /// generic (a required formal with no default, and constants derived from
    /// one): expressions referencing them are constant per instance, so their
    /// bare-context fold failure warns misleadingly and is skipped instead.
    per_instance_names: std::collections::HashSet<String>,
    /// User-defined attribute names in scope (`attribute a : T;` in this file
    /// or a `use`d package) — `x'a` lowers to X silently for these.
    attr_decls: std::collections::HashSet<String>,
    /// Counter for path-less file declarations: each gets a unique
    /// `$file:name#n` marker constant, resolved at runtime by file_open.
    file_seq: u64,
}

/// Per-module signal metadata (`SigId` is relative to one module).
#[derive(Default, Clone)]
pub struct SigMaps {
    mems: HashMap<SigId, usize>,
    sig_signed: HashMap<SigId, bool>,
    sig_rec: HashMap<SigId, String>,
    sig_elem_rec: HashMap<SigId, String>,
    sig_width: HashMap<SigId, usize>,
    sig_dims: HashMap<SigId, Vec<(i64, usize)>>,
    sig_asc: HashMap<SigId, (i64, bool)>,
    /// Element width in bits for multi-bit-element vectors (`string`: 8) —
    /// see [`Lower::sig_elem`]. Mirrored with the entity's other signal maps
    /// so an architecture addresses entity-declared strings correctly.
    sig_elem: HashMap<SigId, usize>,
    uncon_ports: std::collections::HashSet<SigId>,
    /// Constants and types declared in the entity's declarative part — an
    /// architecture sees them (the entity's declarative region encompasses
    /// its architectures, LRM 1076 §1.2).
    consts: HashMap<String, Value>,
    types: HashMap<String, TypeInfo>,
    /// `obj_dims` recorded in the entity's declarative part (generic and
    /// constant shapes) — the architecture consults them for
    /// `'length`-family folds on those names.
    obj_dims: ObjDims,
    /// Aliases (and their record types) declared in the entity's declarative
    /// part — visible in its architectures too. `SigId`s are module-relative
    /// and the architecture reuses the entity module's signal table, so the
    /// ids need no remapping.
    aliases: HashMap<String, Expr>,
    alias_rec: HashMap<String, String>,
}

impl SigMaps {
    /// JSON view of the diagnosis-relevant lower-side maps: widths, array
    /// dims, memory depths, ascending flags, unconstrained ports, and the
    /// entity-declarative constants. Types and aliases are omitted (not
    /// dim-related); this feeds `hdlsim ir --dump-maps`.
    pub fn debug_json(&self) -> serde_json::Value {
        serde_json::json!({
            "sig_width": self.sig_width,
            "sig_dims": self.sig_dims,
            "mems": self.mems,
            "sig_asc": self.sig_asc,
            "sig_signed": self.sig_signed,
            "uncon_ports": self.uncon_ports,
            "consts": self.consts,
        })
    }
}

impl<'a> Lower<'a> {
    fn new(db: &'a SourceDb, diags: &'a mut Vec<Diagnostic>) -> Lower<'a> {
        Lower {
            db,
            diags,
            consts: Rc::new(HashMap::new()),
            const_types: HashMap::new(),
            funcs: HashMap::new(),
            merged_all: std::collections::HashSet::new(),
            funcs_ce: std::rc::Rc::new(HashMap::new()),
            types: HashMap::new(),
            type_widths: HashMap::new(),
            type_elem: HashMap::new(),
            type_dims: HashMap::new(),
            names: HashMap::new(),
            mems: HashMap::new(),
            sig_signed: HashMap::new(),
            sig_rec: HashMap::new(),
            pkg_sig_types: HashMap::new(),
            pkg_sig_widths: HashMap::new(),
            sig_elem_rec: HashMap::new(),
            sig_width: HashMap::new(),
            sig_dims: HashMap::new(),
            obj_dims: HashMap::new(),
            sig_asc: HashMap::new(),
            sig_elem: HashMap::new(),
            sig_dynw: HashMap::new(),
            agg_target_w: None,
            rec_field_dims: HashMap::new(),
            access_inner: HashMap::new(),
            rec_field_ty: HashMap::new(),
            phys_units: HashMap::new(),
            names_stack: Vec::new(),
            extra_signals: Vec::new(),
            extra_base: 0,
            inst_count: 0,
            uncon_ports: std::collections::HashSet::new(),
            fn_formals: std::collections::HashSet::new(),
            aliases: HashMap::new(),
            alias_rec: HashMap::new(),
            suppress_err: 0,
            fn_scope: 0,
            pkgs: HashMap::new(),
            cfgs: HashMap::new(),
            file_root: None,
            inst_packages: Vec::new(),
            entity_maps: HashMap::new(),
            entity_maps_by_dir: HashMap::new(),
            entity_consts: HashMap::new(),
            entity_types: HashMap::new(),
            entity_obj_dims: HashMap::new(),
            entity_aliases: HashMap::new(),
            entity_alias_rec: HashMap::new(),
            attr_decls: std::collections::HashSet::new(),
            variant: None,
            cur_entity: String::new(),
            cur_scope: String::new(),
            str_ctx: false,
            per_instance_names: std::collections::HashSet::new(),
            prot_types: HashMap::new(),
            prot_body_ctx: None,
            sig_prot: HashMap::new(),
            pending_funcs: Vec::new(),
            file_seq: 0,
        }
    }

    /// Snapshot the current signal maps (called after an entity's ports are
    /// lowered) and clear them so the next entity starts fresh.
    fn snapshot_entity_maps(&mut self, entity: &str) {
        let maps = SigMaps {
            mems: self.mems.clone(),
            sig_signed: self.sig_signed.clone(),
            sig_rec: self.sig_rec.clone(),
            sig_elem_rec: self.sig_elem_rec.clone(),
            sig_width: self.sig_width.clone(),
            sig_dims: self.sig_dims.clone(),
            sig_asc: self.sig_asc.clone(),
            sig_elem: self.sig_elem.clone(),
            uncon_ports: self.uncon_ports.clone(),
            consts: self.entity_consts.clone(),
            types: self.entity_types.clone(),
            obj_dims: self.entity_obj_dims.clone(),
            aliases: self.entity_aliases.clone(),
            alias_rec: self.entity_alias_rec.clone(),
        };
        self.entity_maps.insert(entity.to_string(), maps.clone());
        // Dir-keyed twin: the entity's own source directory disambiguates
        // same-named entities from different libraries/directories.
        if let Some(dir) = self.file_root.as_ref().and_then(|r| file_dir(self.db, r)) {
            self.entity_maps_by_dir
                .insert((dir, entity.to_ascii_lowercase()), maps);
        }
        self.mems.clear();
        self.sig_signed.clear();
        self.sig_rec.clear();
        self.sig_elem_rec.clear();
        self.sig_width.clear();
        self.sig_dims.clear();
        self.sig_asc.clear();
        self.sig_elem.clear();
        self.uncon_ports.clear();
    }

    /// Install the signal maps of the entity whose architecture is being
    /// lowered (SigId is module-relative). The (directory, entity) entry wins:
    /// a sibling entity's snapshot is the correct scope even when another
    /// directory declared an entity with the same name first.
    fn install_entity_maps(&mut self, entity: &str) {
        let dir_key = self
            .file_root
            .as_ref()
            .and_then(|r| file_dir(self.db, r))
            .map(|d| (d, entity.to_ascii_lowercase()));
        let m = dir_key
            .as_ref()
            .and_then(|k| self.entity_maps_by_dir.get(k))
            .cloned()
            .or_else(|| self.entity_maps.get(entity).cloned())
            .unwrap_or_default();
        self.mems = m.mems;
        self.sig_signed = m.sig_signed;
        self.sig_rec = m.sig_rec;
        self.sig_elem_rec = m.sig_elem_rec;
        self.sig_width = m.sig_width;
        self.sig_dims = m.sig_dims;
        self.sig_asc = m.sig_asc;
        self.sig_elem = m.sig_elem;
        self.uncon_ports = m.uncon_ports;
        for (k, v) in m.consts {
            Rc::make_mut(&mut self.consts).entry(k).or_insert(v);
        }
        for (k, v) in m.types {
            self.types.entry(k).or_insert(v);
        }
        for (k, v) in m.obj_dims {
            self.obj_dims.entry(k).or_insert(v);
        }
    }

    /// Merge the packages named in the file's context clauses into scope
    /// (`use lib.pkg.all;` merges everything, `use lib.pkg.item;` just that item).
    fn apply_context_clauses(&mut self, root: &SyntaxNode) {
        for child in real_children(root) {
            if matches!(
                child.kind(),
                SyntaxKind::ContextClause | SyntaxKind::UseClause
            ) {
                self.merge_use_clause(&child);
            }
        }
        // The const-eval fallback (`funcs_ce`) is shared design-wide
        // (`Design::pkg_funcs`) — see the field docs — so nothing to
        // snapshot here: rebuilding it per context application re-cloned the
        // whole package closure (quadratic in design size).
    }

    /// Merge one `use lib.pkg.item;` / `context lib.pkg;` clause into scope.
    fn merge_use_clause(&mut self, u: &SyntaxNode) {
        for n in real_children(u) {
            if n.kind() == SyntaxKind::Name {
                let text = self.db.span_text(n.span()).trim();
                let parts: Vec<&str> = text.split('.').collect();
                // Two-part names are ambiguous: `lib.unit` (e.g.
                // `context vunit_lib.com_context;`) vs `unit.item` / `unit.all`.
                // If the second part names a known package, it is the unit.
                let (unit, item) = match parts.len() {
                    n if n >= 3 => (parts[n - 2], parts[n - 1]),
                    2 if parts[1].eq_ignore_ascii_case("all") => (parts[0], parts[1]),
                    2 if self.pkgs.contains_key(&parts[1].to_ascii_lowercase()) => {
                        (parts[1], "all")
                    }
                    2 => (parts[0], parts[1]),
                    _ => continue,
                };
                if let Some(p) = self.pkgs.get(&unit.to_ascii_lowercase()).cloned() {
                    let item = item.to_ascii_lowercase();
                    self.merge_package(&p, if item == "all" { None } else { Some(&item) }, false);
                }
            }
        }
    }

    fn err(&mut self, span: Span, msg: impl Into<String>) {
        if self.suppress_err > 0 {
            return;
        }
        let msg = msg.into();
        self.diags.push(Diagnostic {
            code: "E0401",
            severity: Severity::Error,
            span: Some(span),
            message: msg,
            help: None,
        });
    }

    fn warn(&mut self, span: Span, msg: impl Into<String>) {
        self.diags.push(Diagnostic {
            code: "W0401",
            severity: Severity::Warning,
            span: Some(span),
            message: msg.into(),
            help: None,
        });
    }

    fn text(&self, n: &SyntaxNode) -> String {
        self.db.span_text(n.span()).trim().to_string()
    }

    fn kw(&self, n: &SyntaxNode) -> String {
        self.text(n).to_ascii_lowercase()
    }

    /// Index of the formal name of a named association (`formal => actual`),
    /// or `None` for a positional one. The lexer emits `=>` as two
    /// single-char puncts, so the arrow is the first adjacent `=` `>` pair.
    /// A top-level association expression never yields a bare `=` followed
    /// by `>` (binary ops parse into expr nodes; `>=` lexes in reverse
    /// order), so the first pair is the association arrow when present.
    fn assoc_formal(&self, ck: &[SyntaxNode]) -> Option<usize> {
        let arrow = ck.windows(2).position(|w| {
            w[0].kind() == SyntaxKind::Punct
                && self.text(&w[0]) == "="
                && w[1].kind() == SyntaxKind::Punct
                && self.text(&w[1]) == ">"
        })?;
        ck[..arrow]
            .iter()
            .rposition(|c| c.kind() == SyntaxKind::Name)
    }

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
    fn register_func(&mut self, f: FuncIr) {
        let cands = self.funcs.entry(f.name.to_ascii_lowercase()).or_default();
        let sig = |c: &FuncIr| {
            c.args.len() == f.args.len()
                && c.args.iter().map(|a| a.1).eq(f.args.iter().map(|a| a.1))
        };
        let is_stub = |c: &FuncIr| match &c.body {
            Stmt::Null => true,
            Stmt::Block(ss) => ss.is_empty(),
            _ => false,
        };
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
        if super::debug::TRACE_OVERLOAD.load(std::sync::atomic::Ordering::Relaxed)
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

    fn merge_package(&mut self, p: &PackageIr, item: Option<&str>, force: bool) {
        let mut visited = std::mem::take(&mut self.merged_all);
        self.merge_package_rec(p, item, force, &mut visited);
        self.merged_all = visited;
    }

    fn merge_package_rec(
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
    fn register_package_inst(&mut self, d: &SyntaxNode) {
        if let Some(q) = self.lower_package_inst(d) {
            let key = q.name.to_ascii_lowercase();
            self.merge_package(&q, None, true);
            self.pkgs.insert(key.clone(), Rc::new(q.clone()));
            self.inst_packages.push(q);
        }
    }

    fn lower_package(
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
    fn lower_package_inst(&mut self, node: &SyntaxNode) -> Option<PackageIr> {
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

    /// Lower an architecture body onto its entity's module. Returns false
    /// (without emitting a diagnostic) when the entity is not in `entities` —
    /// the caller either defers the architecture or reports E0401 via
    /// [`Lower::err_unknown_entity`].
    /// Lower an architecture body. On success the body is attached to a
    /// per-architecture variant module `entity/arch` (pushed to `modules`)
    /// and the function returns `(entity, arch)`; the plain entity module gets
    /// the default architecture's body afterwards (see `apply_default_archs`).
    /// Returns None when the entity is unknown (caller defers or errors).
    fn lower_architecture(
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
    fn lower_conc_stmt(&mut self, module: &mut ModuleIr, s: &SyntaxNode, proc_idx: &mut usize) {
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
    fn gen_body(&self, after: &[SyntaxNode]) -> (Vec<SyntaxNode>, Vec<SyntaxNode>) {
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
    fn lower_gen_decls(&mut self, module: &mut ModuleIr, decls: &[SyntaxNode], fresh: bool) {
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
    fn lower_gen_for(&mut self, module: &mut ModuleIr, s: &SyntaxNode, proc_idx: &mut usize) {
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
        if super::debug::TRACE_GENRANGE.load(std::sync::atomic::Ordering::Relaxed) && n > 64 {
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
    fn lower_gen_if(&mut self, module: &mut ModuleIr, s: &SyntaxNode, proc_idx: &mut usize) {
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
    fn lower_gen_case(&mut self, module: &mut ModuleIr, s: &SyntaxNode, proc_idx: &mut usize) {
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
    fn lower_scoped(
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
    fn lower_block(&mut self, module: &mut ModuleIr, s: &SyntaxNode, proc_idx: &mut usize) {
        let kids = real_children(s);
        let (decls, stmts) = self.gen_body(&kids);
        // Block-header ports are handled by the PortClause arm in
        // lower_gen_decls: they become signals of the enclosing module.
        self.lower_scoped(module, &decls, &stmts, proc_idx);
    }

    /// E0401 for an architecture naming an entity that is unknown design-wide:
    /// the caret points at the entity name itself (not the whole architecture
    /// node), with a did-you-mean help when a known entity name is close.
    fn err_unknown_entity(&mut self, a: &SyntaxNode, candidates: &[String]) {
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

    // ---- declarations ------------------------------------------------------------

    /// `alias name is target;` — a whole-signal target is bound in `names`
    /// (so sensitivity lists etc. see it), a constant target in `consts`,
    /// anything else (slice/index select) in the expression-alias table.
    fn lower_alias_decl(&mut self, d: &SyntaxNode) {
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

    fn lower_signal_decl(&mut self, module: &mut ModuleIr, d: &SyntaxNode) {
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
                        },
                        origin: format!("init protected object {n}"),
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
    fn lower_file_decl(&mut self, d: &SyntaxNode) {
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

    fn lower_constant_decl(&mut self, d: &SyntaxNode) {
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
        // …`): register the object's dims so `'length(d)`-style attributes on
        // the constant fold (the constraint is not part of the type mark).
        if let (Some(tm), Some(c)) = (&type_mark, &constraint) {
            let tn = self
                .text(tm)
                .rsplit('.')
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            if matches!(self.types.get(&tn), Some(TypeInfo::Array { .. }))
                && !self.text(c).contains("<>")
            {
                let cd = self.constraint_dims(c);
                if !cd.is_empty() {
                    for n in &names {
                        self.obj_dims.insert(n.to_ascii_lowercase(), cd.clone());
                    }
                }
            }
        }
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
                } else {
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
    fn lower_fn_local_constant(
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
    fn protected_method_stub(&mut self, f: &SyntaxNode) {
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
            locals: Vec::new(),
            dyn_locals: Vec::new(),
            body: Stmt::Null,
        });
    }

    // ---- processes and statements ---------------------------------------------------

    /// Lower an architecture-level function declaration to FuncIr. The body is
    /// lowered in slot space: slot 0 = function name (return value), then
    /// parameters, then local variables.
    fn lower_function_decl(&mut self, module: &mut ModuleIr, f: &SyntaxNode) {
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
    fn subprogram_is_function(&self, f: &SyntaxNode) -> bool {
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
    fn subprogram_stub(&self, f: &SyntaxNode) -> Option<FuncIr> {
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
    fn dyn_width_expr(&mut self, tm: &SyntaxNode, c: &SyntaxNode) -> Expr {
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
    fn dyn_others_init(&mut self, node: &SyntaxNode, wexpr: &Expr) -> Option<Expr> {
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

    fn lower_function_node(&mut self, f: &SyntaxNode) -> Option<FuncIr> {
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
        let mut ret_width = 32usize;
        // Params: PortDecl `a, b : integer [range] [:= default]` — names
        // before ':', type after.
        let mut param_w: HashMap<String, usize> = HashMap::new();
        let mut args: Vec<(String, usize, String)> = Vec::new();
        let mut arg_types: Vec<(String, String)> = Vec::new();
        // Per-formal type mark + constraint for `record_obj_shape` — the
        // formal's element width (`string` → 8) and index direction must be
        // known while the body lowers (`arg'length`, slices over locals).
        let mut arg_shapes: Vec<(String, Option<SyntaxNode>, Option<SyntaxNode>)> = Vec::new();
        let mut defaults: Vec<Option<Value>> = Vec::new();
        for pd in decl_nodes
            .iter()
            .filter(|d| d.kind() == SyntaxKind::PortDecl)
        {
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
                .filter(|k| k.kind() == SyntaxKind::Range);
            // `b : a'subtype` — the base is an earlier formal of this
            // subprogram (VHDL-2019 subprogram generics); adopt its width.
            let (w, _, _, _) = match &tmark {
                Some(tm) if param_w.contains_key(&self.kw(tm)) => {
                    (param_w[&self.kw(tm)], false, None, false)
                }
                Some(tm) => self.resolve_type(tm, constraint, pd.span()),
                None => (32, false, None, false),
            };
            // The declared type name disambiguates same-width overloads at
            // dispatch (see FlatFunc::formal_types).
            let ty = tmark
                .as_ref()
                .map(|tm| self.text(tm).to_ascii_lowercase())
                .unwrap_or_default();
            // Interface default (`a, b : natural := 3`): const-eval now and
            // record one entry per formal of this decl — a call may omit the
            // corresponding actual. Defaults are static expressions; one
            // referencing a formal, a later declaration, or a name from a
            // package hdlsim does not have (ieee.fixed_pkg formal styles)
            // stays unrecorded (an omitted actual then reads X, as before
            // this feature). Lower leniently: a diagnostic here would fail
            // files that never omit these actuals. `:=` lexes as the `=`
            // punct (same as declaration initializers).
            let default_v = pk
                .iter()
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
                .map(|v| v.resize(w.max(1)));
            for n in names {
                param_w.insert(n.to_ascii_lowercase(), w);
                if let Some(tm) = &tmark {
                    arg_types.push((n.clone(), self.kw(tm)));
                }
                arg_shapes.push((n.clone(), tmark.clone(), constraint.cloned()));
                args.push((n, w, ty.clone()));
                defaults.push(default_v.clone());
            }
        }
        // Return type mark resolved after the parameter loop so a
        // `return a'subtype` mark (base = an earlier formal) finds its width.
        if let Some(tm) = &ret_mark {
            ret_width = match param_w.get(&self.kw(tm)) {
                Some(w) => *w,
                None => self.resolve_type(tm, None, f.span()).0,
            };
        }
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
        // Formals' element widths and index directions: `arg'length` on a
        // string formal must report an element count (the eval sides report
        // the bound value's bit width), and slice remaps over a dynamic-width
        // local read the direction. Captured/restored with the other
        // slot-id maps below — slot ids alias module signal ids.
        let saved_shape_elem: HashMap<SigId, Option<usize>> = (0..slots.len())
            .map(|i| {
                let id = i as SigId;
                (id, self.sig_elem.get(&id).copied())
            })
            .collect();
        let saved_shape_asc: HashMap<SigId, Option<(i64, bool)>> = (0..slots.len())
            .map(|i| {
                let id = i as SigId;
                (id, self.sig_asc.get(&id).copied())
            })
            .collect();
        let saved_shape_dynw: HashMap<SigId, Option<Expr>> = (0..slots.len())
            .map(|i| {
                let id = i as SigId;
                (id, self.sig_dynw.get(&id).cloned())
            })
            .collect();
        let saved_shape_dims: HashMap<SigId, Option<Vec<(i64, usize)>>> = (0..slots.len())
            .map(|i| {
                let id = i as SigId;
                (id, self.sig_dims.get(&id).cloned())
            })
            .collect();
        // Slot ids alias across subprograms: a previously lowered
        // subprogram's shape entries (`sig_asc`, `sig_elem`, `sig_dynw`,
        // `sig_dims`) under the same id would mis-address this one's slices
        // and fold its attributes from another function's object. Drop them
        // for the slots about to be registered; the captures above restore
        // the entry state at exit, and `record_obj_shape` re-adds what this
        // subprogram's formals actually have.
        for i in 0..slots.len() {
            let id = i as SigId;
            self.sig_asc.remove(&id);
            self.sig_elem.remove(&id);
            self.sig_dynw.remove(&id);
            self.sig_dims.remove(&id);
        }
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
        // same-numbered slot.
        // Slot ids share the flat-signal id space: module signals (ports)
        // can hold the same ids, and function formals must not leak their
        // record mappings into the enclosing scope after the body is
        // lowered (a later same-numbered non-record formal would otherwise
        // inherit this function's record — see the reset after the body).
        // Capture the entry state so the exit reset restores it exactly.
        let saved_rec: HashMap<SigId, Option<String>> = (0..slots.len())
            .map(|i| {
                let id = i as SigId;
                (id, self.sig_rec.get(&id).cloned())
            })
            .collect();
        let saved_elem: HashMap<SigId, Option<String>> = (0..slots.len())
            .map(|i| {
                let id = i as SigId;
                (id, self.sig_elem_rec.get(&id).cloned())
            })
            .collect();
        let saved_prot: HashMap<SigId, Option<String>> = (0..slots.len())
            .map(|i| {
                let id = i as SigId;
                (id, self.sig_prot.get(&id).cloned())
            })
            .collect();
        for (i, (n, _)) in slots.iter().enumerate() {
            let tn = arg_types
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
                SyntaxKind::VariableDecl => {
                    let dk = real_children(d);
                    let Some(colon) = dk
                        .iter()
                        .position(|k| k.kind() == SyntaxKind::Punct && self.text(k) == ":")
                    else {
                        continue;
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
                        if node.kind() == SyntaxKind::Aggregate
                            && tmark.is_some_and(|tm| self.string_typed(tm))
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
                _ => {}
            }
        }
        // Record-typed locals (declared in the loop above): same treatment.
        for (i, (n, _)) in slots.iter().enumerate() {
            let tn = local_types
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
        for (id, v) in saved_rec {
            match v {
                Some(v) => {
                    self.sig_rec.insert(id, v);
                }
                None => {
                    self.sig_rec.remove(&id);
                }
            }
        }
        for (id, v) in saved_elem {
            match v {
                Some(v) => {
                    self.sig_elem_rec.insert(id, v);
                }
                None => {
                    self.sig_elem_rec.remove(&id);
                }
            }
        }
        for (id, v) in saved_prot {
            match v {
                Some(v) => {
                    self.sig_prot.insert(id, v);
                }
                None => {
                    self.sig_prot.remove(&id);
                }
            }
        }
        for (id, v) in saved_shape_elem {
            match v {
                Some(v) => {
                    self.sig_elem.insert(id, v);
                }
                None => {
                    self.sig_elem.remove(&id);
                }
            }
        }
        for (id, v) in saved_shape_asc {
            match v {
                Some(v) => {
                    self.sig_asc.insert(id, v);
                }
                None => {
                    self.sig_asc.remove(&id);
                }
            }
        }
        for (id, v) in saved_shape_dynw {
            match v {
                Some(v) => {
                    self.sig_dynw.insert(id, v);
                }
                None => {
                    self.sig_dynw.remove(&id);
                }
            }
        }
        for (id, v) in saved_shape_dims {
            match v {
                Some(v) => {
                    self.sig_dims.insert(id, v);
                }
                None => {
                    self.sig_dims.remove(&id);
                }
            }
        }
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

    fn lower_process(&mut self, module: &mut ModuleIr, p: &SyntaxNode, idx: usize) -> ProcessIr {
        let kids = real_children(p);
        let mut sens = Sens::List(Vec::new());
        let mut stmt_nodes: Vec<SyntaxNode> = Vec::new();
        let mut local_fns: Vec<SyntaxNode> = Vec::new();
        for k in &kids {
            match k.kind() {
                SyntaxKind::SensList => {
                    sens = self.lower_sens_list(k);
                }
                SyntaxKind::Function if self.subprogram_is_function(k) => {
                    if let Some(stub) = self.subprogram_stub(k) {
                        self.register_func(stub);
                    }
                    local_fns.push(k.clone());
                }
                SyntaxKind::SignalDecl
                | SyntaxKind::VariableDecl
                | SyntaxKind::ConstantDecl
                | SyntaxKind::TypeDecl
                | SyntaxKind::SubtypeDecl
                | SyntaxKind::AliasDecl
                | SyntaxKind::AttrDecl => {
                    // Process-local declarations.
                    match k.kind() {
                        SyntaxKind::VariableDecl => {
                            self.lower_process_var(module, k, idx);
                        }
                        SyntaxKind::ConstantDecl => {
                            self.lower_constant_decl(k);
                            self.materialize_const(module, k);
                        }
                        SyntaxKind::TypeDecl => self.lower_type_decl(k),
                        SyntaxKind::SubtypeDecl => self.lower_subtype_decl(k),
                        SyntaxKind::AliasDecl => self.lower_alias_decl(k),
                        SyntaxKind::AttrDecl => {
                            self.register_attr_decl(k);
                        }
                        _ => {}
                    }
                }
                SyntaxKind::UseClause => self.merge_use_clause(k),
                SyntaxKind::PackageInst => self.register_package_inst(k),
                SyntaxKind::FileDecl => self.lower_file_decl(k),
                _ if is_stmt_node(k.kind()) => stmt_nodes.push(k.clone()),
                _ => {}
            }
        }
        // Process-local functions (`function prefix return string is ...`):
        // register them so bare parameterless calls (`prefix & "..."`) resolve.
        for k in &local_fns {
            if let Some(fi) = self.lower_function_node(k) {
                self.register_func(fi);
            }
        }
        self.extra_base = module.signals.len();
        self.extra_signals.clear();
        let body = if stmt_nodes.len() == 1 {
            self.lower_seq_stmt(&stmt_nodes[0])
        } else {
            Stmt::Block(stmt_nodes.iter().map(|k| self.lower_seq_stmt(k)).collect())
        };
        for sig in self.extra_signals.drain(..) {
            module.signals.push(sig);
        }
        // VHDL processes always loop: with a sensitivity list they re-run on
        // each event (`always @(sens)`); without one they re-run from the top
        // whenever they reach the end, suspending only at wait statements
        // (empty sens list = bare `always`, re-queued immediately).
        let kind = ProcKind::Always;
        // Name the process by source location in diagnostics — an AI reading
        // the transcript can jump straight to the source. (The parser drops
        // `label :` before building the node, so labels don't reach the CST.)
        let sp = p.span();
        let origin = {
            let f = self.db.file(sp.file);
            format!(
                "process #{} at {}:{}",
                idx,
                f.name(),
                f.line_col(sp.start).0
            )
        };
        ProcessIr {
            kind,
            sens,
            body,
            origin,
            // VHDL processes execute once at t=0 (LRM 11.3), unlike a
            // Verilog `always @(...)` which waits for the first event.
            initial: true,
        }
    }

    /// A process-local variable becomes a (hidden) signal — VHDL variables
    /// keep their value between activations, exactly like a signal.
    fn lower_process_var(&mut self, module: &mut ModuleIr, d: &SyntaxNode, proc_idx: usize) {
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
        let constraint = kids[colon + 1..]
            .iter()
            .find(|k| k.kind() == SyntaxKind::Range);
        // The initializer follows `:=` and may be a bare name (constant or
        // parameterless call) — see `lower_signal_decl`.
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
        let (width, signed, _, real) = match type_mark {
            Some(tm) => self.resolve_type(tm, constraint, span),
            None => (32, false, None, false),
        };
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
            let id = module.signals.len() as SigId;
            module.signals.push(SignalIr {
                name: format!("__p{}_var_{}", proc_idx, n),
                width,
                kind: if real { SigKind::Real } else { SigKind::Wire },
                init: init_v.clone(),
                mem: None,
                signed,
            });
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
                if let Some(en) = self.as_elem_record(tn) {
                    self.sig_elem_rec.insert(id, en);
                }
                // Protected-type object local: member selects placeholder.
                if matches!(self.types.get(tn), Some(TypeInfo::Handle)) {
                    self.sig_prot.insert(id, tn.clone());
                }
                if let Some(TypeInfo::Array { dims, .. }) = self.types.get(tn) {
                    if !dims.is_empty() {
                        let dd = match constraint {
                            Some(c) => self.constraint_dims(c),
                            None => dims.clone(),
                        };
                        self.sig_dims.insert(id, dd);
                    }
                }
            }
            self.names.insert(n.clone(), id);
        }
    }

    fn lower_sens_list(&mut self, s: &SyntaxNode) -> Sens {
        let mut edges = Vec::new();
        for c in real_children(s) {
            // The parser wraps postfix indexing (`arr(i)`, `data(7 downto 0)`)
            // in a Call/PartSelect node with the base Name nested first, so a
            // bare `SyntaxKind::Name` check silently drops indexed entries —
            // leaving the process with an empty sensitivity list, which the
            // sim treats as a bare `always` (infinite zero-delay re-run).
            // LRM 1076.4 puts the longest static prefix of each name in the
            // sensitivity set: for `arr(i)` that is `arr` itself.
            let name_node = match c.kind() {
                SyntaxKind::Name => Some(c.clone()),
                SyntaxKind::Call | SyntaxKind::PartSelect => real_children(&c)
                    .into_iter()
                    .find(|k| k.kind() == SyntaxKind::Name),
                _ => None,
            };
            let Some(name_node) = name_node else { continue };
            let name = self.text(&name_node);
            {
                // VHDL-2008 `process(all)`: sensitive to every signal read
                // (case-insensitive — vendor code writes `PROCESS (ALL)`).
                if name.eq_ignore_ascii_case("all") {
                    return Sens::All;
                }
                // Selected names (`bus_req_i.addr`, `arr(i).stb`) — sensitive to
                // the whole base signal.
                let base_name = name
                    .split(['.', '('])
                    .next()
                    .unwrap_or(name.as_str())
                    .to_string();
                let lookup = if base_name != name { &base_name } else { &name };
                if let Some(id) = self.sig_id(lookup) {
                    edges.push(Edge {
                        sig: id,
                        kind: EdgeKind::Any,
                    });
                } else if self
                    .pkg_sig_types
                    .contains_key(&lookup.to_ascii_lowercase())
                {
                    // Package-level signal (e.g. `signal runner` in a package
                    // declaration, used in `wait on runner`): placeholder.
                    let id = self.ensure_global_sig(lookup);
                    edges.push(Edge {
                        sig: id,
                        kind: EdgeKind::Any,
                    });
                } else {
                    self.err(
                        name_node.span(),
                        format!("unknown signal '{}' in sensitivity list", name),
                    );
                }
            }
        }
        Sens::List(edges)
    }

    /// Concurrent `assert cond report msg severity lvl;` → an always-process
    /// sensitive to the condition's signals, with the sequential assert as
    /// body. Runs once at t=0 like any VHDL process.
    fn lower_conc_assert(&mut self, s: &SyntaxNode, idx: usize) -> ProcessIr {
        let body = self.lower_seq_stmt_impl(s);
        let mut sigs = Vec::new();
        if let Stmt::SysCall { args, .. } = &body {
            for a in args.iter().take(1) {
                a.collect_sigs(&mut sigs);
            }
        }
        ProcessIr {
            kind: ProcKind::Always,
            sens: Sens::List(
                sigs.into_iter()
                    .map(|sig| Edge {
                        sig,
                        kind: EdgeKind::Any,
                    })
                    .collect(),
            ),
            body,
            origin: format!("concurrent assert #{idx}"),
            initial: true,
        }
    }

    /// `unaffected` waveform element (a bare keyword child of the assignment
    /// statement, not an expression node).
    fn is_unaffected_val(&self, n: &SyntaxNode) -> bool {
        n.kind() == SyntaxKind::Ident && self.kw(n) == "unaffected"
    }

    /// One waveform element: `expr [after t] [when c]`.
    fn waveform_elems(&self, kids: &[SyntaxNode], start: usize) -> Vec<WaveElem> {
        // Waveform elements: `expr [after t] [when c]`, comma-separated, or the
        // conditional form `expr when c else expr [when c]... [else expr]`.
        let mut elems: Vec<WaveElem> = Vec::new();
        let mut cur: Option<WaveElem> = None;
        let mut i = start;
        while i < kids.len() {
            let k = &kids[i];
            match self.kw(k).as_str() {
                "after" | "when" => {
                    if let Some(e) = kids.get(i + 1).filter(|e| is_expr_node(e.kind())) {
                        if let Some(c) = cur.as_mut() {
                            if self.kw(k) == "after" {
                                c.delay = Some(e.clone());
                            } else {
                                c.cond = Some(e.clone());
                            }
                        }
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                "else" => {
                    if let Some(c) = cur.take() {
                        elems.push(c);
                    }
                    if let Some(e) = kids.get(i + 1).filter(|e| is_expr_node(e.kind())) {
                        cur = Some(WaveElem {
                            val: e.clone(),
                            delay: None,
                            cond: None,
                        });
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                "unaffected" if k.kind() == SyntaxKind::Ident => {
                    if let Some(c) = cur.take() {
                        elems.push(c);
                    }
                    cur = Some(WaveElem {
                        val: k.clone(),
                        delay: None,
                        cond: None,
                    });
                    i += 1;
                }
                _ if is_expr_node(k.kind()) => {
                    if let Some(c) = cur.take() {
                        elems.push(c);
                    }
                    cur = Some(WaveElem {
                        val: k.clone(),
                        delay: None,
                        cond: None,
                    });
                    i += 1;
                }
                _ => i += 1,
            }
        }
        if let Some(c) = cur {
            elems.push(c);
        }
        elems
    }

    fn lower_conc_assign(&mut self, s: &SyntaxNode, idx: usize) -> Option<ConcOut> {
        let kids = real_children(s);
        let lhs_pos = kids.iter().position(|k| {
            k.kind() == SyntaxKind::Name
                || k.kind() == SyntaxKind::Call
                || k.kind() == SyntaxKind::PartSelect
        })?;
        let lhs = self.lower_expr(&kids[lhs_pos], &self.consts.clone());
        let elems = self.waveform_elems(&kids, lhs_pos + 1);
        if elems.is_empty() {
            return None;
        }
        let has_cond = elems.iter().any(|e| e.cond.is_some());
        let has_delay = elems.iter().any(|e| e.delay.is_some());
        if has_delay {
            // VHDL waveform: each element's delay is relative to the time the
            // statement executes. `v after t` schedules a transaction at
            // now + t WITHOUT suspending the process (`AssignDelayed`), so the
            // equivalent process (LRM 11.3)
            //   `process begin <assigns>; wait on <rhs signals>; end process;`
            // re-arms on every RHS event — self-oscillators
            // (`clk <= not clk after 10 ns;`) keep ticking.
            let mut stmts = Vec::new();
            let mut rhs_sigs: Vec<SigId> = Vec::new();
            for e in &elems {
                // `unaffected` element: schedules no transaction.
                if self.is_unaffected_val(&e.val) {
                    continue;
                }
                let mut val = self.lower_rhs(&kids[lhs_pos], &e.val, &self.consts.clone());
                if let Some(c) = &e.cond {
                    // `v after t when c` — fold the condition into the value.
                    val = Expr::Cond {
                        c: Box::new(self.lower_expr(c, &self.consts.clone())),
                        t: Box::new(val),
                        e: Box::new(Expr::Lit(Value::x(1))),
                    };
                }
                val.collect_sigs(&mut rhs_sigs);
                let t_fs = match &e.delay {
                    Some(d) => {
                        let dv = self.lower_expr(d, &self.consts.clone());
                        match self.ce(&dv) {
                            Some(v) => v.to_u64().unwrap_or(0), // X (unoverridden generic) -> 0
                            None => {
                                // Non-constant delay (e.g. `after Baud/2` with Baud a
                                // signal): not schedulable exactly — apply the
                                // transaction with no delay instead of dropping the
                                // whole assignment.
                                self.warn(d.span(), "non-constant waveform delay; treated as 0");
                                0
                            }
                        }
                    }
                    None => 0,
                };
                stmts.push(Stmt::AssignDelayed {
                    lhs: lhs.clone(),
                    rhs: val,
                    delay: Expr::Lit(Value::from_u64(64, t_fs)),
                });
            }
            if stmts.is_empty() {
                return None; // every element `unaffected`: no assignment
            }
            let body = if stmts.len() == 1 {
                stmts.pop().expect("checked stmts.len() == 1")
            } else {
                Stmt::Block(stmts)
            };
            if rhs_sigs.is_empty() {
                // A waveform reading no signal: schedule once, no wake source.
                return Some(ConcOut::Proc(ProcessIr {
                    kind: ProcKind::Initial,
                    sens: Sens::List(Vec::new()),
                    body,
                    origin: format!("concurrent assign #{}", idx),
                    initial: false,
                }));
            }
            // Loop with a trailing `wait on <rhs signals>` so each RHS event
            // re-executes the assignment. The explicit Wait parks in
            // `self.waiting`; no implicit sens re-park (which would miss the
            // process's own writes).
            let wake = Stmt::Wait {
                sens: rhs_sigs
                    .iter()
                    .map(|s| Edge {
                        sig: *s,
                        kind: EdgeKind::Any,
                    })
                    .collect(),
                body: Box::new(Stmt::Null),
            };
            let body = Stmt::For {
                init: Box::new(Stmt::Null),
                cond: Expr::Lit(Value::from_u64(1, 1)),
                step: Box::new(Stmt::Null),
                body: Box::new(Stmt::Block(vec![body, wake])),
            };
            return Some(ConcOut::Proc(ProcessIr {
                kind: ProcKind::Initial,
                sens: Sens::List(Vec::new()),
                body,
                origin: format!("concurrent assign #{}", idx),
                initial: false,
            }));
        }
        if has_cond {
            // Conditional waveform → nested ternary, first true condition wins.
            let mut rhs: Option<Expr> = None;
            for e in elems.iter().rev() {
                // A trailing `else unaffected` keeps the current value:
                // lower it as a self-assignment (no event).
                let v = if self.is_unaffected_val(&e.val) {
                    lhs.clone()
                } else {
                    self.lower_rhs(&kids[lhs_pos], &e.val, &self.consts.clone())
                };
                rhs = Some(match (&e.cond, rhs) {
                    (Some(c), Some(r)) => Expr::Cond {
                        c: Box::new(self.lower_expr(c, &self.consts.clone())),
                        t: Box::new(v),
                        e: Box::new(r),
                    },
                    (Some(c), None) => Expr::Cond {
                        c: Box::new(self.lower_expr(c, &self.consts.clone())),
                        t: Box::new(v),
                        e: Box::new(Expr::Lit(Value::x(1))),
                    },
                    (None, r) => r.unwrap_or(v),
                });
            }
            return Some(ConcOut::Assign(AssignIr { lhs, rhs: rhs? }));
        }
        // Plain waveform: the last element at t=0 wins (`unaffected`
        // elements add no transaction and are dropped).
        let elems: Vec<WaveElem> = elems
            .into_iter()
            .filter(|e| !self.is_unaffected_val(&e.val))
            .collect();
        let rhs = self.lower_rhs(&kids[lhs_pos], &elems.last()?.val, &self.consts.clone());
        Some(ConcOut::Assign(AssignIr { lhs, rhs }))
    }

    /// `with sel select target <= w1 when c1, w2 when c2, ...;` → a continuous
    /// assign of nested ternaries (first matching choice wins, `others` last).
    fn lower_selected_assign(&mut self, s: &SyntaxNode) -> Option<AssignIr> {
        let kids = real_children(s);
        let sel_pos = kids.iter().position(|k| is_expr_node(k.kind()))?;
        let target_pos = kids[sel_pos + 1..]
            .iter()
            .position(|k| k.kind() == SyntaxKind::Name)?
            + sel_pos
            + 1;
        let sel = self.lower_expr(&kids[sel_pos], &self.consts.clone());
        let lhs = self.lower_expr(&kids[target_pos], &self.consts.clone());
        // Walk `value [after t] when (choice | ... | others) [, ...]` pairs.
        let mut pairs: Vec<(SyntaxNode, Vec<SyntaxNode>, bool)> = Vec::new();
        let mut cur_val: Option<SyntaxNode> = None;
        let mut cur_choices: Vec<SyntaxNode> = Vec::new();
        let mut cur_others = false;
        let mut in_choices = false;
        for k in &kids[target_pos + 1..] {
            match self.kw(k).as_str() {
                "when" => in_choices = true,
                "others" => cur_others = true,
                _ if k.kind() == SyntaxKind::Punct && self.text(k) == "," => {
                    if let Some(v) = cur_val.take() {
                        pairs.push((v, std::mem::take(&mut cur_choices), cur_others));
                        cur_others = false;
                    }
                    in_choices = false;
                }
                _ if is_expr_node(k.kind()) => {
                    if in_choices {
                        cur_choices.push(k.clone());
                    } else {
                        cur_val = Some(k.clone());
                    }
                }
                _ => {}
            }
        }
        if let Some(v) = cur_val {
            pairs.push((v, cur_choices, cur_others));
        }
        if pairs.is_empty() {
            return None;
        }
        let mut rhs: Option<Expr> = None;
        for (val, choices, others) in pairs.iter().rev() {
            let v = self.lower_expr(val, &self.consts.clone());
            if *others {
                rhs = Some(rhs.unwrap_or(v));
                continue;
            }
            let mut cond: Option<Expr> = None;
            for c in choices {
                let e = Expr::Bin {
                    op: BinOp::Eq,
                    l: Box::new(sel.clone()),
                    r: Box::new(self.lower_expr(c, &self.consts.clone())),
                };
                cond = Some(match cond {
                    None => e,
                    Some(prev) => Expr::Bin {
                        op: BinOp::LogOr,
                        l: Box::new(prev),
                        r: Box::new(e),
                    },
                });
            }
            let else_e = rhs.unwrap_or_else(|| Expr::Lit(Value::x(1)));
            rhs = Some(match cond {
                Some(c) => Expr::Cond {
                    c: Box::new(c),
                    t: Box::new(v),
                    e: Box::new(else_e),
                },
                None => else_e,
            });
        }
        Some(AssignIr { lhs, rhs: rhs? })
    }

    fn lower_inst(&mut self, s: &SyntaxNode) -> Option<InstIr> {
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

    /// Lower a run of sequential statements into a Block.
    fn lower_seq_block(&mut self, n: &SyntaxNode) -> Stmt {
        let stmts: Vec<Stmt> = real_children(n)
            .iter()
            .filter(|k| is_stmt_node(k.kind()))
            .map(|k| self.lower_seq_stmt(k))
            .collect();
        if stmts.len() == 1 {
            stmts.into_iter().next().expect("checked stmts.len() == 1")
        } else {
            Stmt::Block(stmts)
        }
    }

    /// Lower one sequential statement, wrapped in a `Stmt::Spanned` with its
    /// source span (drives line/branch coverage).
    fn lower_seq_stmt(&mut self, s: &SyntaxNode) -> Stmt {
        let span = s.span();
        let inner = self.lower_seq_stmt_impl(s);
        Stmt::Spanned {
            span,
            inner: Box::new(inner),
        }
    }

    /// If `l` is a bare state-variable name of the protected body currently
    /// being lowered (and not shadowed by a local/param), return
    /// (type, field, lsb, width) so the assignment becomes a `$setf` write.
    fn prot_field_lhs(&self, l: &SyntaxNode) -> Option<(String, String, usize, usize)> {
        if l.kind() != SyntaxKind::Name {
            return None;
        }
        let key = self.kw(l).to_ascii_lowercase();
        if self.sig_id(&key).is_some() {
            return None; // local/param shadows the field
        }
        let t = self.prot_body_ctx.as_ref()?;
        let pt = self.prot_types.get(t)?;
        let f = pt.fields.iter().find(|(n, _, _, _)| *n == key);
        let (_, w, lsb, _) = f?;
        Some((t.clone(), key, *lsb, *w))
    }

    /// Assignment target rooted at a protected-type state variable but not a
    /// whole field (`st.ptr := …`, `st.idxs(st.idx) := …` with `st` a state
    /// field): cannot use `$setf` — the statement is dropped (placeholder).
    fn prot_nested_lhs(&self, l: &SyntaxNode) -> bool {
        let text = match l.kind() {
            SyntaxKind::Name => self.kw(l),
            SyntaxKind::Call => real_children(l)
                .iter()
                .find(|c| c.kind() == SyntaxKind::Name)
                .map(|c| self.kw(c))
                .unwrap_or_default(),
            _ => return false,
        };
        let base = text.split('.').next().unwrap_or("").to_ascii_lowercase();
        if base.is_empty() || self.sig_id(&base).is_some() {
            return false; // local/param shadows the field
        }
        let Some(t) = self.prot_body_ctx.as_ref() else {
            return false;
        };
        let Some(pt) = self.prot_types.get(t) else {
            return false;
        };
        pt.fields.iter().any(|(n, _, _, _)| *n == base)
    }

    fn lower_seq_stmt_impl(&mut self, s: &SyntaxNode) -> Stmt {
        match s.kind() {
            SyntaxKind::SeqAssign => {
                let kids = real_children(s);
                // `(a, b) := f(...);` — parenthesized target list: not
                // lowered (no error).
                if kids.first().is_some_and(|k| k.kind() == SyntaxKind::Punct) {
                    return Stmt::Null;
                }
                let lhs = kids.iter().find(|k| {
                    matches!(
                        k.kind(),
                        SyntaxKind::Name | SyntaxKind::Call | SyntaxKind::PartSelect
                    )
                });
                let op_pos = kids
                    .iter()
                    .position(|k| k.kind() == SyntaxKind::Punct && self.text(k) == "<");
                let has_when = kids
                    .iter()
                    .any(|k| k.kind() == SyntaxKind::Ident && self.kw(k) == "when");
                let has_after = kids
                    .iter()
                    .any(|k| k.kind() == SyntaxKind::Ident && self.kw(k) == "after");
                if has_after {
                    // `s <= v [after t] [, v2 [after t2] ...] [when c] ...`:
                    // each element is a transaction scheduled at now + its own
                    // delay (VHDL waveform delays are relative to the statement
                    // execution, not cumulative) — the process is not suspended.
                    if let (Some(l), Some(op)) = (lhs, op_pos) {
                        // `unaffected` elements schedule no transaction.
                        let elems: Vec<WaveElem> = self
                            .waveform_elems(&kids, op + 1)
                            .into_iter()
                            .filter(|e| !self.is_unaffected_val(&e.val))
                            .collect();
                        if !elems.is_empty() {
                            let lhs_e = self.lower_expr(l, &self.consts.clone());
                            let mut stmts = Vec::new();
                            for e in &elems {
                                // The delay may be a runtime expression (e.g.
                                // OSVVM `after iDelayVal`): evaluated when the
                                // statement executes.
                                let delay = e
                                    .delay
                                    .as_ref()
                                    .map(|d| self.lower_expr(d, &self.consts.clone()));
                                let val = self.lower_rhs(l, &e.val, &self.consts.clone());
                                let a = match &delay {
                                    None => Stmt::Assign {
                                        lhs: lhs_e.clone(),
                                        rhs: val,
                                        nba: true,
                                    },
                                    Some(dv) => Stmt::AssignDelayed {
                                        lhs: lhs_e.clone(),
                                        rhs: val,
                                        delay: dv.clone(),
                                    },
                                };
                                stmts.push(match &e.cond {
                                    Some(c) => Stmt::If {
                                        cond: self.lower_expr(c, &self.consts.clone()),
                                        then: Box::new(a),
                                        els: None,
                                    },
                                    None => a,
                                });
                            }
                            return if stmts.len() == 1 {
                                stmts.pop().expect("checked stmts.len() == 1")
                            } else {
                                Stmt::Block(stmts)
                            };
                        }
                    }
                    return Stmt::Null;
                }
                if has_when {
                    if let (Some(l), Some(op)) = (lhs, op_pos) {
                        if let Some(e) =
                            self.lower_cond_waveform(&kids[op + 2..], &self.consts.clone())
                        {
                            return Stmt::Assign {
                                lhs: self.lower_expr(l, &self.consts.clone()),
                                rhs: e,
                                nba: true,
                            };
                        }
                    }
                    return Stmt::Null;
                }
                let rhs = kids.iter().rev().find(|k| is_expr_node(k.kind()));
                match (lhs, rhs) {
                    (Some(l), Some(r)) => {
                        if let Some((t, f, lsb, w)) = self.prot_field_lhs(l) {
                            let rv = self.lower_rhs(l, r, &self.consts.clone());
                            return Stmt::SysCall {
                                name: format!("$setf::{}::{}@{},{}", t, f, lsb, w),
                                args: vec![
                                    Expr::Call {
                                        name: "$this".into(),
                                        args: vec![],
                                    },
                                    rv,
                                ],
                            };
                        }
                        if self.prot_nested_lhs(l) {
                            return Stmt::Null;
                        }
                        Stmt::Assign {
                            lhs: self.lower_expr(l, &self.consts.clone()),
                            rhs: self.lower_rhs(l, r, &self.consts.clone()),
                            nba: true, // VHDL signal assignment is delta-delayed
                        }
                    }
                    _ => Stmt::Null,
                }
            }
            SyntaxKind::VarAssign => {
                let kids = real_children(s);
                let lhs = kids.iter().find(|k| {
                    k.kind() == SyntaxKind::Name
                        || k.kind() == SyntaxKind::Call
                        || k.kind() == SyntaxKind::PartSelect
                });
                // Conditional expression: `v := a when c else b;`
                let has_when = kids
                    .iter()
                    .any(|k| k.kind() == SyntaxKind::Ident && self.kw(k) == "when");
                if has_when {
                    let colon_pos = kids
                        .iter()
                        .position(|k| k.kind() == SyntaxKind::Punct && self.text(k) == ":");
                    if let (Some(l), Some(cp)) = (lhs, colon_pos) {
                        if let Some(e) =
                            self.lower_cond_waveform(&kids[cp + 2..], &self.consts.clone())
                        {
                            return Stmt::Assign {
                                lhs: self.lower_expr(l, &self.consts.clone()),
                                rhs: e,
                                nba: false,
                            };
                        }
                    }
                    return Stmt::Null;
                }
                let rhs = kids.iter().rev().find(|k| is_expr_node(k.kind()));
                match (lhs, rhs) {
                    (Some(l), Some(r)) => {
                        if let Some((t, f, lsb, w)) = self.prot_field_lhs(l) {
                            let rv = self.lower_rhs(l, r, &self.consts.clone());
                            return Stmt::SysCall {
                                name: format!("$setf::{}::{}@{},{}", t, f, lsb, w),
                                args: vec![
                                    Expr::Call {
                                        name: "$this".into(),
                                        args: vec![],
                                    },
                                    rv,
                                ],
                            };
                        }
                        if self.prot_nested_lhs(l) {
                            return Stmt::Null;
                        }
                        Stmt::Assign {
                            lhs: self.lower_expr(l, &self.consts.clone()),
                            rhs: self.lower_rhs(l, r, &self.consts.clone()),
                            nba: false,
                        }
                    }
                    _ => Stmt::Null,
                }
            }
            SyntaxKind::If => self.lower_if(s),
            SyntaxKind::Case => self.lower_case(s),
            SyntaxKind::For => self.lower_for(s),
            SyntaxKind::LoopStmt => self.lower_loop(s),
            SyntaxKind::Wait => self.lower_wait(s),
            // Sequential block statement: lower its body statements as a
            // block (block-local declarations are not simulated).
            SyntaxKind::Block => {
                let stmts: Vec<Stmt> = real_children(s)
                    .iter()
                    .filter(|k| !is_decl_kind(k.kind()))
                    .filter(|k| !matches!(k.kind(), SyntaxKind::Ident | SyntaxKind::Punct))
                    .map(|k| self.lower_seq_stmt(k))
                    .collect();
                Stmt::Block(stmts)
            }
            SyntaxKind::NullStmt => Stmt::Null,
            SyntaxKind::AssertStmt => {
                // Covers both `assert cond [report msg] [severity lvl]` and
                // `report msg [severity lvl]` (the parser uses one node kind).
                let kids = real_children(s);
                let first_kw = kids
                    .iter()
                    .find(|k| k.kind() == SyntaxKind::Ident)
                    .map(|k| self.kw(k))
                    .unwrap_or_default();
                let severity = kids
                    .iter()
                    .filter(|k| k.kind() == SyntaxKind::Ident)
                    .map(|k| self.kw(k))
                    .find(|w| matches!(w.as_str(), "note" | "warning" | "error" | "failure"))
                    .unwrap_or_default();
                let mut exprs: Vec<Expr> = kids
                    .iter()
                    .filter(|k| is_expr_node(k.kind()))
                    .map(|k| self.lower_expr(k, &self.consts.clone()))
                    .collect();
                if !severity.is_empty() {
                    exprs.push(Expr::Lit(string_value(&severity)));
                }
                Stmt::SysCall {
                    name: if first_kw == "report" {
                        "report"
                    } else {
                        "assert"
                    }
                    .to_string(),
                    args: exprs,
                }
            }
            SyntaxKind::ExitStmt => {
                // The parser folds `exit` and `next` into one ExitStmt node:
                // the first ident child is the keyword. `exit` ends the loop
                // (break), `next` skips to the loop step (continue) — LRM
                // 1076 §10.11/§10.12. Both take an optional `when cond;`.
                let kids = real_children(s);
                let is_next = kids
                    .iter()
                    .find(|k| k.kind() == SyntaxKind::Ident)
                    .map(|k| self.kw(k).eq_ignore_ascii_case("next"))
                    .unwrap_or(false);
                let jump = if is_next { Stmt::Continue } else { Stmt::Break };
                let cond = kids
                    .iter()
                    .find(|k| is_expr_node(k.kind()))
                    .map(|k| self.lower_expr(k, &self.consts.clone()));
                match cond {
                    Some(c) => Stmt::If {
                        cond: c,
                        then: Box::new(jump),
                        els: None,
                    },
                    None => jump,
                }
            }
            SyntaxKind::ReturnStmt => {
                let kids = real_children(s);
                let e = kids.iter().find(|k| is_expr_node(k.kind()));
                Stmt::Return(Box::new(
                    e.map(|e| self.lower_expr(e, &self.consts.clone()))
                        .unwrap_or_else(|| Expr::Lit(Value::from_u64(1, 0))),
                ))
            }
            SyntaxKind::CallStmt => {
                let kids = real_children(s);
                // `foo(a, b);` parses as CallStmt(Call(Name foo, (, args…)));
                // `foo;` as CallStmt(Name foo). Unwrap the Call so the callee
                // and the real arguments are found in both forms.
                let src: Vec<SyntaxNode> = match kids.first() {
                    Some(c) if c.kind() == SyntaxKind::Call => real_children(c),
                    _ => kids.clone(),
                };
                let name_pos = src.iter().position(|k| k.kind() == SyntaxKind::Name);
                let raw = name_pos.map(|i| self.text(&src[i])).unwrap_or_default();
                let fname = raw
                    .rsplit('.')
                    .next()
                    .unwrap_or(raw.as_str())
                    .to_ascii_lowercase();
                let is_textio = TEXTIO_PROCS.contains(&fname.as_str());
                // Association keys (`name => value`) are not arguments: drop
                // a bare Name immediately followed by `=>`. Every other bare
                // Name is a real positional argument — e.g. a variable passed
                // to a user procedure — and must be kept.
                let is_assoc_key: Vec<bool> = src
                    .iter()
                    .enumerate()
                    .map(|(i, k)| {
                        k.kind() == SyntaxKind::Name
                            && src
                                .get(i + 1)
                                .map(|n| self.text(n) == "=>")
                                .unwrap_or(false)
                    })
                    .collect();
                let args: Vec<Expr> = src
                    .iter()
                    .enumerate()
                    .filter(|(i, k)| {
                        Some(*i) != name_pos && is_expr_node(k.kind()) && !is_assoc_key[*i]
                    })
                    .map(|(_, k)| {
                        if k.kind() == SyntaxKind::Name {
                            // A bare-name argument may reference something from
                            // an unseen package/context — lower leniently to a
                            // placeholder, like port-map association actuals.
                            self.suppress_err += 1;
                            let e = self.lower_expr(k, &self.consts.clone());
                            self.suppress_err -= 1;
                            e
                        } else {
                            self.lower_expr(k, &self.consts.clone())
                        }
                    })
                    .collect();
                let name = if is_textio {
                    format!("$textio_{fname}")
                } else {
                    fname
                };
                // `obj.m(args);` on a protected-type object →
                // `$callmethod::t::m(obj, …)`.
                if let Some(dot) = raw.find('.') {
                    let (base, m) = (&raw[..dot], raw[dot + 1..].to_ascii_lowercase());
                    if let Some(id) = self.sig_id(base) {
                        if let Some(t) = self.sig_prot.get(&id).cloned() {
                            let is_method = self
                                .prot_types
                                .get(&t)
                                .is_some_and(|p| p.methods.contains(&m));
                            if is_method {
                                let mut args2 = vec![Expr::Sig(id)];
                                args2.extend(args.iter().cloned());
                                return Stmt::SysCall {
                                    name: format!("$callmethod::{}::{}", t, m),
                                    args: args2,
                                };
                            }
                        }
                    }
                }
                // Bare `m(args);` inside a protected-type body → self-call.
                if let Some(t) = self.prot_body_ctx.clone() {
                    if self
                        .prot_types
                        .get(&t)
                        .is_some_and(|p| p.methods.contains(&name))
                    {
                        let mut args2 = vec![Expr::Call {
                            name: "$this".into(),
                            args: vec![],
                        }];
                        args2.extend(args.iter().cloned());
                        return Stmt::SysCall {
                            name: format!("$callmethod::{}::{}", t, name),
                            args: args2,
                        };
                    }
                }
                // std.env.finish([status]) lowers to a distinct syscall so the
                // kernel can capture the STATUS argument; Verilog `$finish(n)`
                // keeps the plain "finish" name (its argument is a diagnostic
                // level, not an exit status). A user-defined procedure that
                // happens to be named `finish` is treated the same way — the
                // simulation still stops, just without a status.
                let name = if name == "finish" {
                    "$env_finish".to_string()
                } else {
                    name
                };
                Stmt::SysCall { name, args }
            }
            _ => Stmt::Null,
        }
    }

    fn lower_if(&mut self, s: &SyntaxNode) -> Stmt {
        // Children: cond expr, then stmts..., then for each `elsif`: Ident,
        // cond expr, stmts...; an `else`: Ident + stmts....
        let kids = real_children(s);
        let mut branches: Vec<(Option<Expr>, Vec<&SyntaxNode>)> = Vec::new();
        let mut cur_cond: Option<Expr> = None;
        let mut cur_stmts: Vec<&SyntaxNode> = Vec::new();
        let mut pending_cond = false;
        for k in &kids {
            match k.kind() {
                SyntaxKind::Ident => {
                    let w = self.kw(k);
                    if (w == "elsif" || w == "else") && cur_cond.is_some() {
                        branches.push((cur_cond.take(), std::mem::take(&mut cur_stmts)));
                        pending_cond = w == "elsif";
                    }
                }
                _ if is_expr_node(k.kind()) && pending_cond => {
                    cur_cond = Some(self.lower_expr(k, &self.consts.clone()));
                    pending_cond = false;
                }
                _ if is_expr_node(k.kind()) && cur_cond.is_none() => {
                    cur_cond = Some(self.lower_expr(k, &self.consts.clone()));
                }
                _ if is_stmt_node(k.kind()) => cur_stmts.push(k),
                _ => {}
            }
        }
        branches.push((cur_cond, cur_stmts));
        // Fold into nested If from the inside out.
        let mut result = Stmt::Null;
        for (cond, stmts) in branches.into_iter().rev() {
            let body = {
                let ss: Vec<Stmt> = stmts.iter().map(|k| self.lower_seq_stmt(k)).collect();
                if ss.len() == 1 {
                    ss.into_iter().next().expect("checked ss.len() == 1")
                } else {
                    Stmt::Block(ss)
                }
            };
            result = Stmt::If {
                cond: cond.unwrap_or_else(|| Expr::Lit(Value::from_u64(1, 1))),
                then: Box::new(body),
                els: Some(Box::new(result)),
            };
        }
        result
    }

    fn lower_case(&mut self, s: &SyntaxNode) -> Stmt {
        let kids = real_children(s);
        let matching = kids.iter().any(|k| k.kind() == SyntaxKind::Matching);
        let expr = kids.iter().find(|k| is_expr_node(k.kind()));
        let expr = expr
            .map(|k| self.lower_expr(k, &self.consts.clone()))
            .unwrap_or_else(|| Expr::Lit(Value::x(1)));
        let mut items: Vec<(Vec<Expr>, Stmt)> = Vec::new();
        let mut default = None;
        for item in kids.iter().filter(|k| k.kind() == SyntaxKind::CaseItem) {
            let ik = real_children(item);
            let is_others = ik.iter().any(|k| k.kind() == SyntaxKind::Others);
            let conds: Vec<Expr> = ik
                .iter()
                .filter(|k| is_expr_node(k.kind()) && k.kind() != SyntaxKind::Others)
                .map(|k| self.lower_expr(k, &self.consts.clone()))
                .collect();
            let body = self.lower_seq_block(item);
            if is_others {
                default = Some(Box::new(body));
            } else {
                items.push((conds, body));
            }
        }
        Stmt::Case {
            expr,
            items,
            default,
            matching,
        }
    }

    /// `name'range` bounds for a flat value slot (function formal, unsized
    /// signal): `0 to width-1`, resolved from the bound value at evaluation
    /// time via `SigAttr`. Only applies when the slot is not memory-backed
    /// and has no static dims — real array signals keep their static bounds.
    fn flat_slot_range(&self, id: SigId) -> Option<(Expr, Expr)> {
        if self.mems.contains_key(&id) || self.sig_dims.contains_key(&id) {
            return None;
        }
        // The loop variable walks ELEMENTS, so the high bound is the bound
        // value's bit width divided by the element width, minus one — a
        // 64-bit-real formal's 'range yields count-1, not bit-1 (a bit-index
        // bound overran multi-bit-element formals by the element factor).
        // 1-bit elements keep the plain width-1 bound.
        let ew = self.sig_elem.get(&id).copied().unwrap_or(1).max(1) as u64;
        let count = if ew == 1 {
            Expr::SigAttr {
                sig: id,
                attr: "length",
            }
        } else {
            Expr::Bin {
                op: BinOp::Div,
                l: Box::new(Expr::SigAttr {
                    sig: id,
                    attr: "length",
                }),
                r: Box::new(Expr::Lit(Value::from_u64(32, ew))),
            }
        };
        Some((
            Expr::SigAttr {
                sig: id,
                attr: "low",
            },
            Expr::Bin {
                op: BinOp::Sub,
                l: Box::new(count),
                r: Box::new(Expr::Lit(Value::from_u64(32, 1))),
            },
        ))
    }

    fn lower_for(&mut self, s: &SyntaxNode) -> Stmt {
        let kids = real_children(s);
        // for <var> in <range> loop <body> end loop
        let var = kids
            .iter()
            .find(|k| {
                k.kind() == SyntaxKind::Ident
                    && self.kw(k) != "for"
                    && self.kw(k) != "in"
                    && self.kw(k) != "loop"
                    && self.kw(k) != "end"
            })
            .map(|k| self.text(k));
        let range = kids
            .iter()
            .find(|k| k.kind() == SyntaxKind::Range || k.kind() == SyntaxKind::Attr);
        let Some(var) = var else {
            self.err(s.span(), "malformed for loop");
            return Stmt::Null;
        };
        // VHDL for-loop variables are implicitly declared by the loop.
        let var_id = match self.sig_id(&var) {
            Some(id) => id,
            None => {
                let id = (self.extra_base + self.extra_signals.len()) as SigId;
                self.extra_signals.push(SignalIr {
                    name: format!("__for_{}", var),
                    width: 32,
                    kind: SigKind::Wire,
                    init: None,
                    mem: None,
                    signed: false,
                });
                self.names.insert(var.clone(), id);
                id
            }
        };
        // Range bounds.
        let (lo, hi, downto) = match range.map(|r| r.kind()) {
            Some(SyntaxKind::Range) => {
                let rk = real_children(range.expect("matched on range's kind, so range is Some"));
                // `for i in s'range loop` — the range is a Range node holding an
                // Attr child; resolve the bounds from the signal's dims.
                if let Some(at) = rk.iter().find(|k| k.kind() == SyntaxKind::Attr) {
                    let ak = real_children(at);
                    let attr = ak
                        .iter()
                        .rfind(|k| k.kind() == SyntaxKind::Ident)
                        .map(|k| self.kw(k))
                        .unwrap_or_default();
                    if attr == "range" || attr == "reverse_range" {
                        if let Some(b) = ak.iter().find(|k| k.kind() == SyntaxKind::Name) {
                            let bn = self.text(b);
                            // Flat slot (function formal, unsized signal):
                            // runtime bounds from the bound value's width.
                            if let Some((lo, hi)) =
                                self.object_id(&bn).and_then(|id| self.flat_slot_range(id))
                            {
                                let body = self.lower_seq_block(s);
                                return self.make_for(var_id, lo, hi, false, body);
                            }
                            let dims = self
                                .object_id(&bn)
                                .and_then(|id| self.sig_dims.get(&id).cloned())
                                .or_else(|| {
                                    self.types.get(&bn.to_ascii_lowercase()).and_then(|t| {
                                        if let TypeInfo::Array { dims, .. } = t {
                                            Some(dims.clone())
                                        } else {
                                            None
                                        }
                                    })
                                });
                            if let Some(dims) = dims {
                                if let Some(&(lo, n)) = dims.first() {
                                    let hi = lo + n as i64 - 1;
                                    let (a, b, dt) = if attr == "reverse_range" {
                                        (hi, lo, true)
                                    } else {
                                        (lo, hi, false)
                                    };
                                    let body = self.lower_seq_block(s);
                                    return self.make_for(
                                        var_id,
                                        Expr::Lit(Value::from_u64(32, a as u64)),
                                        Expr::Lit(Value::from_u64(32, b as u64)),
                                        dt,
                                        body,
                                    );
                                }
                            }
                            // Memory-backed object without dims (materialized
                            // array constant of an unconstrained type): the
                            // element count is the memory depth; hidden
                            // consts pack element 0 at the top, so the range
                            // is 0 to depth-1.
                            if let Some(depth) = self
                                .object_id(&bn)
                                .and_then(|id| self.mems.get(&id).copied())
                                .filter(|&d| d > 0)
                            {
                                let body = self.lower_seq_block(s);
                                return self.make_for(
                                    var_id,
                                    Expr::Lit(Value::from_u64(32, 0)),
                                    Expr::Lit(Value::from_u64(32, depth as u64 - 1)),
                                    false,
                                    body,
                                );
                            }
                        }
                    }
                }
                let exprs: Vec<&SyntaxNode> =
                    rk.iter().filter(|k| is_expr_node(k.kind())).collect();
                let dir = rk
                    .iter()
                    .find(|k| k.kind() == SyntaxKind::Ident)
                    .map(|k| self.kw(k))
                    .unwrap_or_else(|| "to".to_string());
                let a = exprs
                    .first()
                    .map(|k| self.lower_expr(k, &self.consts.clone()));
                let b = exprs
                    .get(1)
                    .map(|k| self.lower_expr(k, &self.consts.clone()));
                match (a, b) {
                    (Some(a), Some(b)) => {
                        let downto = dir == "downto";
                        if downto {
                            (b, a, true)
                        } else {
                            (a, b, false)
                        }
                    }
                    _ => (
                        Expr::Lit(Value::from_u64(32, 0)),
                        Expr::Lit(Value::from_u64(32, 0)),
                        false,
                    ),
                }
            }
            Some(SyntaxKind::Attr) => {
                // `name'range` on an array signal: 0 to depth-1.
                let ak = real_children(range.expect("matched on range's kind, so range is Some"));
                let base = ak.iter().find(|k| k.kind() == SyntaxKind::Name);
                if let Some(b) = base {
                    let bn = self.text(b);
                    if let Some(id) = self.object_id(&bn) {
                        if let Some(depth) = self.mems.get(&id).copied() {
                            let body = self.lower_seq_block(s);
                            return self.make_for(
                                var_id,
                                Expr::Lit(Value::from_u64(32, 0)),
                                Expr::Lit(Value::from_u64(32, depth as u64 - 1)),
                                false,
                                body,
                            );
                        }
                        // Flat slot (function formal, unsized signal): the
                        // element count comes from the bound value at
                        // runtime / const-eval (`SigAttr`).
                        if let Some((lo, hi)) = self.flat_slot_range(id) {
                            let body = self.lower_seq_block(s);
                            return self.make_for(var_id, lo, hi, false, body);
                        }
                    }
                }
                (
                    Expr::Lit(Value::from_u64(32, 0)),
                    Expr::Lit(Value::from_u64(32, 0)),
                    false,
                )
            }
            _ => (
                Expr::Lit(Value::from_u64(32, 0)),
                Expr::Lit(Value::from_u64(32, 0)),
                false,
            ),
        };
        let body = self.lower_seq_block(s);
        self.make_for(var_id, lo, hi, downto, body)
    }

    fn make_for(&mut self, var_id: SigId, lo: Expr, hi: Expr, downto: bool, body: Stmt) -> Stmt {
        let v = || Expr::Sig(var_id);
        let one = Expr::Lit(Value::from_u64(32, 1));
        let init = Stmt::Assign {
            lhs: v(),
            rhs: lo,
            nba: false,
        };
        let cond = Expr::Bin {
            op: if downto { BinOp::Ge } else { BinOp::Le },
            l: Box::new(v()),
            r: Box::new(hi),
        };
        let step = Stmt::Assign {
            lhs: v(),
            rhs: Expr::Bin {
                op: if downto { BinOp::Sub } else { BinOp::Add },
                l: Box::new(v()),
                r: Box::new(one),
            },
            nba: false,
        };
        Stmt::For {
            init: Box::new(init),
            cond,
            step: Box::new(step),
            body: Box::new(body),
        }
    }

    fn lower_loop(&mut self, s: &SyntaxNode) -> Stmt {
        // `while cond loop body end loop` / `loop body end loop`
        let kids = real_children(s);
        let cond = kids
            .iter()
            .find(|k| is_expr_node(k.kind()))
            .map(|k| self.lower_expr(k, &self.consts.clone()))
            .unwrap_or_else(|| Expr::Lit(Value::from_u64(1, 1)));
        let body = self.lower_seq_block(s);
        Stmt::For {
            init: Box::new(Stmt::Null),
            cond,
            step: Box::new(Stmt::Null),
            body: Box::new(body),
        }
    }

    fn lower_wait(&mut self, s: &SyntaxNode) -> Stmt {
        let kids = real_children(s);
        // wait [until cond] [on sens] [for time];
        let mut cond = None;
        let mut sens = None;
        let mut time_fs = None;
        let mut time_expr = None;
        let mut i = 0;
        while i < kids.len() {
            let k = &kids[i];
            if k.kind() == SyntaxKind::Ident {
                match self.kw(k).as_str() {
                    "until" => {
                        if let Some(e) = kids.get(i + 1).filter(|e| is_expr_node(e.kind())) {
                            cond = Some(self.lower_expr(e, &self.consts.clone()));
                            i += 1;
                        }
                    }
                    "on" => {
                        sens = kids.get(i + 1).cloned();
                        i += 1;
                    }
                    "for" => {
                        // Physical literal: `10 ns`. The parser may merge number
                        // and unit into one Literal token, or leave the unit as
                        // a separate Ident sibling. A non-literal expression
                        // (`for C_TIMEOUT - now`) lowers to an arbitrary time
                        // expression — evaluated when the wait executes.
                        let num = kids.get(i + 1);
                        if let Some(n) = num.filter(|k| k.kind() == SyntaxKind::Literal) {
                            let t = self.text(n);
                            let (v, mult) = match t.split_once(char::is_whitespace) {
                                Some((a, b)) => (
                                    self.parse_int_literal(a.trim())
                                        .and_then(|v| v.to_u64())
                                        .unwrap_or(0),
                                    time_unit_fs(b.trim()),
                                ),
                                None => {
                                    let unit =
                                        kids.get(i + 2).filter(|k| k.kind() == SyntaxKind::Ident);
                                    (
                                        self.parse_int_literal(&t)
                                            .and_then(|v| v.to_u64())
                                            .unwrap_or(0),
                                        unit.map(|k| time_unit_fs(&self.kw(k)))
                                            .unwrap_or(1_000_000),
                                    )
                                }
                            };
                            time_fs = Some(v.saturating_mul(mult));
                            i += 1;
                        } else if let Some(e) = num.filter(|k| is_expr_node(k.kind())) {
                            time_expr = Some(self.lower_expr(e, &self.consts.clone()));
                            i += 1;
                        }
                    }
                    _ => {}
                }
            }
            i += 1;
        }
        // Timeout for `wait until`/`wait on`: literal fs, or the lowered
        // expression (LRM 8.1: the timeout is evaluated when the wait runs).
        let timeout = time_expr.or_else(|| time_fs.map(|fs| Expr::Lit(Value::from_u64(64, fs))));

        // `wait for T;` — pure timed wait: literal → Delay, expression →
        // timeout-only WaitUntil. No until/on clauses.
        if cond.is_none() && sens.is_none() {
            if let Some(t) = time_fs {
                return Stmt::Delay {
                    time_fs: t,
                    stmt: Box::new(Stmt::Null),
                };
            }
            if let Some(te) = timeout {
                return Stmt::WaitUntil {
                    cond: None,
                    sens: Vec::new(),
                    timeout: Some(te),
                };
            }
            // Bare `wait;` — park forever.
            return Stmt::Wait {
                sens: Vec::new(),
                body: Box::new(Stmt::Null),
            };
        }

        // Explicit `on` list wins over cond-derived edges (LRM 8.1).
        let edges = if let Some(sl) = sens.as_ref() {
            if sl.kind() == SyntaxKind::SensList {
                Some(match self.lower_sens_list(sl) {
                    Sens::List(es) => es,
                    Sens::All => Vec::new(),
                })
            } else {
                None
            }
        } else {
            None
        };
        let edges = edges.or_else(|| cond.as_ref().and_then(|c| edges_from_cond(c, self)));

        // A pure `rising_edge(s)`/`falling_edge(s)` condition is fully encoded
        // by its edge kind — no re-check. Any other condition wakes on any
        // event of its signals and re-checks the full expression (slightly
        // over-sensitive vs the LRM, but never misses the true wake).
        let recheck = match &cond {
            Some(Expr::Rising(inner)) | Some(Expr::Falling(inner)) => {
                !matches!(inner.as_ref(), Expr::Sig(_))
            }
            _ => true,
        };
        if edges.is_none() && timeout.is_none() {
            // No wake source at all: the process can never resume.
            self.warn(
                s.span(),
                "wait statement has no sensitivity, condition signals, or timeout — it can never resume",
            );
        }
        Stmt::WaitUntil {
            cond: if recheck { cond } else { None },
            sens: edges.unwrap_or_default(),
            timeout,
        }
    }

    // ---- expressions ---------------------------------------------------------------

    /// Lower an array aggregate against element type `tn` with `n` elements
    /// flat-indexed from `lo`: named index choices (`0 => v`) place by
    /// value, `others` fills the rest, positional elements fill in order.
    /// Every element is resized to the element width; the result concatenates
    /// element 0 first (flat convention: element 0 = MSB).
    fn lower_array_aggregate(
        &mut self,
        e: &SyntaxNode,
        tn: &str,
        n: usize,
        lo: i64,
        env: &HashMap<String, Value>,
    ) -> Option<Expr> {
        let Some(TypeInfo::Array {
            elem, elem_name, ..
        }) = self.types.get(tn).cloned()
        else {
            return None;
        };
        self.lower_array_aggregate_specs(e, n, lo, elem, elem_name, env)
    }

    /// Aggregate assigned to an array-typed signal: same element-ordinal
    /// layout as [`Self::lower_array_aggregate`] (element `lo` first = most
    /// significant), driven by the signal's declared dims/depth instead of
    /// a type name.
    fn lower_array_aggregate_for_sig(
        &mut self,
        e: &SyntaxNode,
        id: SigId,
        env: &HashMap<String, Value>,
    ) -> Option<Expr> {
        let (lo, n) = match self.sig_dims.get(&id) {
            Some(d) if !d.is_empty() => (d[0].0, d[0].1.max(1)),
            _ => (0, (*self.mems.get(&id)?).max(1)),
        };
        // `sig_width` is the element width for memory-backed signals, the
        // flat width otherwise — divide out the element count there.
        let ew = if self.mems.contains_key(&id) {
            (*self.sig_width.get(&id)?).max(1)
        } else {
            (*self.sig_width.get(&id)?).max(1).saturating_div(n)
        };
        let elem_rec = self.sig_elem_rec.get(&id).cloned();
        self.lower_array_aggregate_specs(e, n, lo, ew.max(1), elem_rec, env)
    }

    /// Field layout of a record type name: (field name, lsb offset, width)
    /// per field, in declaration order (first field = MSB of the flat vector).
    fn record_fields_of_type(&self, type_name: &str) -> Vec<(String, usize, usize)> {
        let Some(TypeInfo::Record { fields }) = self.types.get(type_name) else {
            return Vec::new();
        };
        let total: usize = fields.iter().map(|f| f.1).sum();
        let mut run = 0usize;
        let mut out = Vec::new();
        for (fn_, w, _) in fields {
            out.push((fn_.clone(), total - run - w, *w));
            run += w;
        }
        out
    }

    /// Field layout of a record-typed LHS node, if it is one.
    fn record_fields_of_lhs(&self, lhs: &SyntaxNode) -> Option<Vec<(String, usize, usize)>> {
        if lhs.kind() != SyntaxKind::Name {
            return None;
        }
        let name = self.text(lhs);
        let base = name.split('.').next().unwrap_or("").to_ascii_lowercase();
        let id = self.sig_id(&base)?;
        let tn = self.sig_rec.get(&id)?;
        let out = self.record_fields_of_type(tn);
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    }

    /// Lower a record aggregate: named associations (`field => value`) are
    /// placed by declaration order, positional ones in order; each field value
    /// is resized to the field width. Unassigned fields (no `others`) → X.
    fn lower_record_aggregate(
        &mut self,
        e: &SyntaxNode,
        fields: &[(String, usize, usize)],
        env: &HashMap<String, Value>,
    ) -> Expr {
        let kids = real_children(e);
        let mut named: Vec<(String, usize)> = Vec::new();
        let mut positional: Vec<usize> = Vec::new();
        let mut i = 0;
        while i < kids.len() {
            let k = &kids[i];
            let arrow = kids
                .get(i + 1)
                .map(|p| p.kind() == SyntaxKind::Punct && self.text(p) == "=")
                .unwrap_or(false)
                && kids
                    .get(i + 2)
                    .map(|p| p.kind() == SyntaxKind::Punct && self.text(p) == ">")
                    .unwrap_or(false);
            if arrow {
                named.push((self.text(k).to_ascii_lowercase(), i + 3));
                i += 4;
            } else if is_expr_node(k.kind()) {
                positional.push(i);
                i += 1;
            } else {
                i += 1;
            }
        }
        let others = named.iter().find(|(n, _)| n == "others").map(|(_, v)| *v);
        let mut parts: Vec<Expr> = Vec::new();
        for (fi, (fname, _, fw)) in fields.iter().enumerate() {
            let vi = named
                .iter()
                .find(|(n, _)| n == fname)
                .map(|(_, v)| *v)
                .or(others);
            let raw = match vi {
                Some(vi) => self.lower_expr(&kids[vi], env),
                None => positional
                    .get(fi)
                    .map(|&pi| self.lower_expr(&kids[pi], env))
                    .unwrap_or_else(|| Expr::Lit(Value::x(*fw))),
            };
            parts.push(Expr::Resize {
                e: Box::new(raw),
                width: *fw,
                signed: false,
            });
        }
        Expr::Concat(parts)
    }

    /// Lower an assignment RHS, using the LHS record type (if any) to order
    /// named aggregate associations.
    /// Lower a flat conditional waveform / expression
    /// (`v0 when c0 else v1 when c1 else v2`) into nested `Expr::Cond`.
    fn lower_cond_waveform(
        &mut self,
        kids: &[SyntaxNode],
        env: &HashMap<String, Value>,
    ) -> Option<Expr> {
        let mut pairs: Vec<(Expr, Option<Expr>)> = Vec::new();
        let mut cur_val: Option<Expr> = None;
        let mut cur_cond: Option<Expr> = None;
        let mut i = 0;
        while i < kids.len() {
            let k = &kids[i];
            if k.kind() == SyntaxKind::Ident {
                match self.kw(k).as_str() {
                    "when" => {
                        if let Some(c) = kids.get(i + 1).filter(|e| is_expr_node(e.kind())) {
                            cur_cond = Some(self.lower_expr(c, env));
                            i += 2;
                            continue;
                        }
                    }
                    "else" => {
                        if let Some(v) = cur_val.take() {
                            pairs.push((v, cur_cond.take()));
                        }
                        if let Some(e) = kids.get(i + 1).filter(|e| is_expr_node(e.kind())) {
                            cur_val = Some(self.lower_expr(e, env));
                            i += 2;
                            continue;
                        }
                    }
                    _ => {}
                }
            } else if is_expr_node(k.kind()) {
                if let Some(v) = cur_val.take() {
                    pairs.push((v, cur_cond.take()));
                }
                cur_val = Some(self.lower_expr(k, env));
            }
            i += 1;
        }
        if let Some(v) = cur_val {
            pairs.push((v, cur_cond));
        }
        let mut acc: Option<Expr> = None;
        for (v, c) in pairs.into_iter().rev() {
            acc = Some(match c {
                Some(c) => Expr::Cond {
                    c: Box::new(c),
                    t: Box::new(v),
                    e: Box::new(acc.unwrap_or(Expr::Lit(Value::x(1)))),
                },
                None => v,
            });
        }
        acc
    }

    fn lower_rhs(
        &mut self,
        lhs: &SyntaxNode,
        rhs: &SyntaxNode,
        env: &HashMap<String, Value>,
    ) -> Expr {
        if rhs.kind() == SyntaxKind::Aggregate {
            if let Some(fields) = self.record_fields_of_lhs(lhs) {
                return self.lower_record_aggregate(rhs, &fields, env);
            }
            // Array-typed target: named choices (`0 => '1', others => '0`)
            // must place their elements — the width-only fallback keeps
            // only `others` and turns the rest into the others-value.
            if let Some(id) = self.array_lhs_sig(lhs) {
                if let Some(ex) = self.lower_array_aggregate_for_sig(rhs, id, env) {
                    return ex;
                }
            }
        }
        // A plain OTHERS aggregate into a string target replicates over the
        // target's whole bit width (`(others => ' ')` filled only 4 of 5
        // characters with the baked 32-bit fallback — see `agg_target_w`).
        let saved_w = self.agg_target_w;
        if rhs.kind() == SyntaxKind::Aggregate {
            if let Some(id) = self.sig_id(&self.text(lhs)) {
                if self.sig_elem.get(&id) == Some(&8) {
                    if let Some(w) = self.sig_width.get(&id) {
                        self.agg_target_w = Some(*w);
                    }
                }
            }
        }
        let ex = self.lower_expr(rhs, env);
        self.agg_target_w = saved_w;
        ex
    }

    /// The signal id behind an assignment target whose type is an array
    /// (declared dims or memory-backed) — the anchor for typed aggregate
    /// lowering of the RHS.
    fn array_lhs_sig(&self, lhs: &SyntaxNode) -> Option<SigId> {
        if lhs.kind() != SyntaxKind::Name {
            return None;
        }
        let name = self.text(lhs);
        let base = name.split('.').next().unwrap_or("").to_ascii_lowercase();
        let id = self.sig_id(&base)?;
        if self.sig_dims.contains_key(&id) || self.mems.contains_key(&id) {
            Some(id)
        } else {
            None
        }
    }

    /// Parse a VHDL integer literal: decimal (`1_000`) or based (`16#FF#E1`).
    fn parse_int_literal(&mut self, t: &str) -> Option<Value> {
        let t = t.trim();
        if let Some(h) = t.find('#') {
            let base: u32 = t[..h].parse().ok()?;
            if !(2..=36).contains(&base) {
                return None; // invalid base (`0#...#`) must not panic
            }
            let rest = &t[h + 1..];
            let end = rest.find('#')?;
            let digits = rest[..end].replace('_', "");
            let exp = rest[end + 1..]
                .strip_prefix(['e', 'E'])
                .and_then(|e| e.parse::<i32>().ok())
                .unwrap_or(0);
            let mut v = u128::from_str_radix(&digits, base).ok()?;
            if exp > 0 {
                v = v.checked_mul((base as u128).checked_pow(exp as u32)?)?;
            }
            let width = (128 - v.leading_zeros() as usize).max(1);
            return Some(value_from_u128(width, v));
        }
        let digits = t.replace('_', "");
        let v = digits.parse::<u128>().ok()?;
        // Plain decimal literals are VHDL integers: 32-bit, so `i + 1` does
        // not sign-extend the 1-bit literal `1` (based literals like 16#FF#
        // keep their minimal width and are usually bit patterns).
        Some(value_from_u128(32, v))
    }
}

/// Levenshtein edit distance, counted in chars.
#[cfg(test)]
fn levenshtein(a: &str, b: &str) -> usize {
    crate::levenshtein_bounded(a, b, usize::MAX)
}

/// String as a Value (first char in the high byte, matching `lower_literal`).
pub(crate) fn string_value(s: &str) -> Value {
    let bytes = s.as_bytes();
    let n = bytes.len().max(1);
    let mut bits = vec![V0; n * 8];
    for (bi, &b) in bytes.iter().enumerate() {
        for j in 0..8 {
            bits[(n - 1 - bi) * 8 + j] = if (b >> j) & 1 == 1 { V1 } else { V0 };
        }
    }
    Value { bits, real: false }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_libraries_collects_library_wide_uses() {
        let src = "library math;\nuse math.all;\nuse math.math.elog2;\nuse ieee.std_logic_1164.all;\nentity t is end;\n";
        let mut db = SourceDb::new();
        let id = db.add_file("t.vhd".to_string(), src.to_string()).unwrap();
        let parsed = hdl_syntax::vhdl::parser::parse_file(id, db.file(id));
        assert_eq!(
            context_libraries(&db, &parsed.root),
            vec!["math".to_string()]
        );
        // 3-part names still yield unit names, not libraries.
        assert!(context_units(&db, &parsed.root).contains(&"math".to_string()));
        assert!(context_units(&db, &parsed.root).contains(&"std_logic_1164".to_string()));
    }

    #[test]
    fn did_you_mean_prefers_close_names() {
        let cands = ["edge_detector_entity".to_string(), "fifo".to_string()];
        // Prefix overlap: `edge_detector` is a prefix of `edge_detector_entity`.
        assert_eq!(
            did_you_mean("edge_detector", cands.iter()),
            Some("edge_detector_entity")
        );
        // Small edit distance.
        assert_eq!(did_you_mean("fifo2", cands.iter()), Some("fifo"));
        // Case-insensitive exact match is not suggested.
        assert_eq!(did_you_mean("FIFO", cands.iter()), None);
        // No close candidate at all.
        assert_eq!(did_you_mean("zzzzzz", cands.iter()), None);
        assert_eq!(did_you_mean("", cands.iter()), None);
    }

    #[test]
    fn levenshtein_distance_counts_chars() {
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("abc", "abc"), 0);
        assert_eq!(levenshtein("", "ab"), 2);
    }

    #[test]
    fn access_type_index_and_deref_lower_cleanly() {
        // `l(0).all` — index into an access-to-array auto-derefs, `.all`
        // derefs the element access; folded into one Name node by the parser.
        let src = "package p is\n  type line_t is access string;\n  type line_vec_t is array (natural range <>) of line_t;\n  type lines_t is access line_vec_t;\n  function mk return lines_t;\n  function g return string;\nend package;\npackage body p is\n  function mk return lines_t is\n    variable v : lines_t;\n  begin\n    return v;\n  end function;\n  function g return string is\n    variable l : lines_t := mk;\n  begin\n    return l(0).all;\n  end function;\nend package body;\n";
        let mut db = SourceDb::new();
        let id = db.add_file("t.vhd".to_string(), src.to_string()).unwrap();
        let parsed = hdl_syntax::vhdl::parser::parse_file(id, db.file(id));
        let mut diags = Vec::new();
        let _ = lower_file(
            id,
            &parsed.root,
            &db,
            &mut diags,
            1_000_000,
            &HashMap::new(),
            &std::rc::Rc::new(HashMap::new()),
            &HashMap::new(),
        );
        let errs: Vec<_> = diags
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .collect();
        assert!(
            errs.is_empty(),
            "{:?}",
            errs.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn nested_record_field_via_used_package_resolves() {
        // `m2s.ar.valid` — the record field type map must cross package
        // boundaries (PackageIr::rec_field_tys).
        let pkg = "package axi_pkg is\n  type ar_t is record\n    valid : bit;\n  end record;\n  type rd_t is record\n    ar : ar_t;\n  end record;\nend package;\n";
        let top = "use work.axi_pkg.all;\nentity top is\n  port (q : out bit);\nend entity;\narchitecture rtl of top is\n  signal m2s : rd_t;\nbegin\n  q <= m2s.ar.valid;\nend architecture;\n";
        let mut db = SourceDb::new();
        let id1 = db.add_file("p.vhd".to_string(), pkg.to_string()).unwrap();
        let parsed1 = hdl_syntax::vhdl::parser::parse_file(id1, db.file(id1));
        let mut diags = Vec::new();
        let mut pkgs = HashMap::new();
        let lowered = lower_file(
            id1,
            &parsed1.root,
            &db,
            &mut diags,
            1_000_000,
            &HashMap::new(),
            &std::rc::Rc::new(HashMap::new()),
            &HashMap::new(),
        );
        for p in lowered.packages {
            pkgs.insert(p.name.to_ascii_lowercase(), std::rc::Rc::new(p));
        }
        let id2 = db.add_file("top.vhd".to_string(), top.to_string()).unwrap();
        let parsed2 = hdl_syntax::vhdl::parser::parse_file(id2, db.file(id2));
        let _ = lower_file(
            id2,
            &parsed2.root,
            &db,
            &mut diags,
            1_000_000,
            &pkgs,
            &pkg_funcs_from(&pkgs),
            &HashMap::new(),
        );
        let errs: Vec<_> = diags
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .collect();
        assert!(
            errs.is_empty(),
            "{:?}",
            errs.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    /// Design-wide const-eval fallback for tests, mirroring how hdl-lib
    /// maintains `Design::pkg_funcs` as packages arrive.
    fn pkg_funcs_from(
        pkgs: &HashMap<String, std::rc::Rc<PackageIr>>,
    ) -> std::rc::Rc<HashMap<String, Vec<std::rc::Rc<FuncIr>>>> {
        let mut m: HashMap<String, Vec<std::rc::Rc<FuncIr>>> = HashMap::new();
        for p in pkgs.values() {
            for f in &p.functions {
                m.entry(f.name.to_ascii_lowercase())
                    .or_default()
                    .push(std::rc::Rc::new(f.clone()));
            }
        }
        std::rc::Rc::new(m)
    }

    fn lower_src(src: &str) -> Vec<hdl_syntax::diagnostic::Diagnostic> {
        let mut db = SourceDb::new();
        let id = db.add_file("t.vhd".to_string(), src.to_string()).unwrap();
        let parsed = hdl_syntax::vhdl::parser::parse_file(id, db.file(id));
        let mut diags = Vec::new();
        let _ = lower_file(
            id,
            &parsed.root,
            &db,
            &mut diags,
            1_000_000,
            &HashMap::new(),
            &std::rc::Rc::new(HashMap::new()),
            &HashMap::new(),
        );
        diags
    }

    fn assert_no_errors(diags: &[hdl_syntax::diagnostic::Diagnostic]) {
        let errs: Vec<_> = diags
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .collect();
        assert!(
            errs.is_empty(),
            "{:?}",
            errs.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn process_local_function_bare_call_resolves() {
        // `function prefix return string` inside a process, called bare.
        let diags = lower_src("entity top is port (q : out bit); end entity;\narchitecture rtl of top is\nbegin\n  process\n    function prefix return string is\n    begin\n      return \"x\";\n    end function prefix;\n  begin\n    report prefix & \"done\";\n    wait;\n  end process;\nend architecture;\n");
        assert_no_errors(&diags);
    }

    #[test]
    fn process_local_procedure_with_port_record_field_is_skipped() {
        // A process-local *procedure* (no `return`) whose body selects a field
        // of a record-typed entity port: procedures are not lowered (the
        // subprogram slot space does not see outer-scope names), so this must
        // not produce "unknown identifier" diagnostics.
        let diags = lower_src("package p is\n  type rec_t is record readdata : std_logic_vector; writedata : std_logic_vector; end record;\nend package;\nuse work.p.all;\nentity top is\n  port (amr : inout rec_t; rdata : in std_logic_vector(31 downto 0); wdata : out std_logic_vector(31 downto 0));\nend entity;\narchitecture rtl of top is\nbegin\n  process\n    procedure wr is\n    begin\n      amr.readdata <= rdata;\n      wdata <= amr.writedata;\n    end procedure;\n  begin\n    wr;\n    wait;\n  end process;\nend architecture;\n");
        assert_no_errors(&diags);
    }

    #[test]
    fn record_param_field_in_call_arg_resolves() {
        // `get(q.p_meta, 0)` — field select on a record parameter inside a
        // constant initializer in the function's declarative part.
        let diags = lower_src("package p is\n  type inner_t is record v : integer; end record;\n  type q_t is record p_meta : inner_t; end record;\n  function get(x : integer; i : natural) return integer;\n  function get_meta(q : q_t) return integer;\nend package;\npackage body p is\n  function get(x : integer; i : natural) return integer is begin return x; end function;\n  function get_meta(q : q_t) return integer is\n    constant head : integer := get(q.p_meta, 0);\n  begin return head; end function;\nend package body;\n");
        assert_no_errors(&diags);
    }

    #[test]
    fn protected_type_member_select_is_placeholder() {
        // `self.data_size` on a protected-typed shared variable.
        let diags = lower_src("package p is\n  type rec_t is protected\n    impure function get return natural;\n  end protected;\nend package;\npackage body p is\n  type rec_t is protected body\n    impure function get return natural is begin return 1; end;\n  end protected body;\nend package body;\nuse work.p.all;\nentity top is port (q : out bit); end entity;\narchitecture rtl of top is\n  shared variable self : rec_t;\nbegin\n  process begin\n    q <= bit'val(self.data_size > 0);\n    wait;\n  end process;\nend architecture;\n");
        assert_no_errors(&diags);
    }

    #[test]
    fn record_typed_generic_field_select_resolves() {
        // `bus_handle.p_logger` where bus_handle is a record-typed generic.
        let diags = lower_src("package p is\n  type logger_t is record id : integer; end record;\n  type bus_t is record p_logger : logger_t; end record;\nend package;\nuse work.p.all;\nentity top is\n  generic (bus_handle : bus_t);\n  port (q : out bit);\nend entity;\narchitecture rtl of top is\nbegin\n  q <= bit'val(bus_handle.p_logger.id = 0);\nend architecture;\n");
        assert_no_errors(&diags);
    }

    #[test]
    fn protected_pkg_shared_var_member_select_via_use_is_placeholder() {
        // `messenger.timeout_is_allowed` / `messenger.get_all_actors` where
        // messenger is a package-level shared variable of a protected type.
        let mut db = SourceDb::new();
        let id1 = db.add_file("p.vhd".to_string(), "package p is\n  type messenger_t is protected\n    impure function get_all_actors return integer;\n  end protected;\n  shared variable messenger : messenger_t;\nend package;\n".to_string()).unwrap();
        let parsed1 = hdl_syntax::vhdl::parser::parse_file(id1, db.file(id1));
        let mut diags = Vec::new();
        let mut pkgs = HashMap::new();
        let lowered = lower_file(
            id1,
            &parsed1.root,
            &db,
            &mut diags,
            1_000_000,
            &HashMap::new(),
            &std::rc::Rc::new(HashMap::new()),
            &HashMap::new(),
        );
        for p in lowered.packages {
            pkgs.insert(p.name.to_ascii_lowercase(), std::rc::Rc::new(p));
        }
        let id2 = db.add_file("top.vhd".to_string(), "use work.p.all;\npackage body p is\n  function f return boolean is\n    constant actors : integer := messenger.get_all_actors;\n  begin\n    return messenger.timeout_is_allowed or actors > 0;\n  end;\nend package body;\n".to_string()).unwrap();
        let parsed2 = hdl_syntax::vhdl::parser::parse_file(id2, db.file(id2));
        let _ = lower_file(
            id2,
            &parsed2.root,
            &db,
            &mut diags,
            1_000_000,
            &pkgs,
            &pkg_funcs_from(&pkgs),
            &HashMap::new(),
        );
        assert_no_errors(&diags);
    }

    #[test]
    fn pkg_signal_alias_exported_and_usable_in_sens_list() {
        // `alias runner_phase is runner(...)` in a package with a
        // package-level signal: the alias must be visible in using files,
        // and the signal in a sensitivity list (`wait on runner`).
        let mut db = SourceDb::new();
        let id1 = db.add_file("p.vhd".to_string(), "package p is\n  signal runner : bit_vector(7 downto 0);\n  alias runner_phase is runner(3 downto 0);\nend package;\n".to_string()).unwrap();
        let parsed1 = hdl_syntax::vhdl::parser::parse_file(id1, db.file(id1));
        let mut diags = Vec::new();
        let mut pkgs = HashMap::new();
        let lowered = lower_file(
            id1,
            &parsed1.root,
            &db,
            &mut diags,
            1_000_000,
            &HashMap::new(),
            &std::rc::Rc::new(HashMap::new()),
            &HashMap::new(),
        );
        for p in lowered.packages {
            pkgs.insert(p.name.to_ascii_lowercase(), std::rc::Rc::new(p));
        }
        let id2 = db.add_file("top.vhd".to_string(), "use work.p.all;\nentity top is port (q : out bit); end entity;\narchitecture rtl of top is\nbegin\n  process\n  begin\n    wait on runner until runner_phase = \"0000\";\n    q <= '1';\n    wait;\n  end process;\nend architecture;\n".to_string()).unwrap();
        let parsed2 = hdl_syntax::vhdl::parser::parse_file(id2, db.file(id2));
        let _ = lower_file(
            id2,
            &parsed2.root,
            &db,
            &mut diags,
            1_000_000,
            &pkgs,
            &pkg_funcs_from(&pkgs),
            &HashMap::new(),
        );
        assert_no_errors(&diags);
    }

    #[test]
    fn nested_function_referenced_in_constant_initializer() {
        // `constant result : string := create_string;` where create_string is
        // a function nested in the same subprogram (stub pre-registration).
        let diags = lower_src("package p is\nend package;\npackage body p is\n  function outer(s : string) return string is\n    impure function create_string return string is\n    begin\n      return s;\n    end;\n    constant result : string := create_string;\n  begin\n    return result;\n  end;\nend package body;\n");
        assert_no_errors(&diags);
    }

    #[test]
    fn block_level_function_bare_call_resolves() {
        // `impure function next_outstanding ...` declared in a block's
        // declarative part, called bare from a process inside the block.
        let diags = lower_src("entity top is port (clk : in bit; q : out natural); end entity;\narchitecture a of top is\n  signal outstanding : natural := 0;\nbegin\n  write_block : block\n    impure function next_outstanding return natural is\n    begin\n      return outstanding + 1;\n    end;\n  begin\n    main : process\n    begin\n      wait until rising_edge(clk);\n      outstanding <= next_outstanding;\n      q <= outstanding;\n    end process;\n  end block;\nend architecture;\n");
        assert_no_errors(&diags);
    }

    #[test]
    fn param_dependent_local_function_lowers_without_warning() {
        // `variable l : bit_vector(a'length - 1 downto 0);` inside a function
        // (as in Vivado's std_logic_arith): the function must still lower and
        // no "parameter-dependent" warning may be emitted (issue #106).
        let diags = lower_src("package p is\n  function f(a : bit_vector) return bit_vector;\nend package;\npackage body p is\n  function f(a : bit_vector) return bit_vector is\n    variable l : bit_vector(a'length - 1 downto 0);\n  begin\n    l := a;\n    return l;\n  end;\nend package body;\n");
        assert_no_errors(&diags);
        let msgs: Vec<&str> = diags.iter().map(|d| d.message.as_str()).collect();
        assert!(
            msgs.iter().all(|m| !m.contains("parameter-dependent")),
            "{msgs:?}"
        );
    }

    #[test]
    fn loop_built_vector_in_constant_initializer_const_evals() {
        // `constant c := ones_vec(3);` where ones_vec fills the vector in a
        // for loop: the const evaluator must splice indexed assignments into
        // the local slot — this was the last remaining W0401 from the issue
        // #106 report (a testbench `constant c := generate_test_vector;`).
        let diags = lower_src("package p is\n  function ones_vec(n : integer) return bit_vector;\nend package;\npackage body p is\n  function ones_vec(n : integer) return bit_vector is\n    variable v : bit_vector(0 to n - 1);\n  begin\n    for i in 0 to n - 1 loop\n      v(i) := '1';\n    end loop;\n    return v;\n  end function;\nend package body;\nentity e is end entity;\narchitecture a of e is\n  constant c : bit_vector := work.p.ones_vec(3);\n  signal s : bit := c(0);\nbegin\nend architecture;\n");
        let msgs: Vec<String> = diags.iter().map(|d| d.message.clone()).collect();
        assert!(diags.is_empty(), "{msgs:?}");
    }

    #[test]
    fn param_dependent_local_constants_lower_without_warning() {
        // std_logic_arith-style function-local constants (issue #106
        // follow-up): `constant rMsb : integer := SIZE - 1;` and
        // `constant msb : integer := min(ARG'length, SIZE) - 1;` are valid
        // VHDL. They lower as per-call runtime slots (initializer evaluated
        // on every call), so no "not constant; treated as X" warning.
        let diags = lower_src("package p is\n  type nat_mat is array (natural range <>, natural range <>) of natural;\n  function f(size : integer; m : nat_mat) return integer;\nend package;\npackage body p is\n  function f(size : integer; m : nat_mat) return integer is\n    constant rmsb : integer := size - 1;\n    constant msb  : integer := m'length(1) - size;\n    constant xsz  : integer := m'length(2);\n  begin\n    return rmsb + msb + xsz;\n  end function;\nend package body;\n");
        let msgs: Vec<String> = diags.iter().map(|d| d.message.clone()).collect();
        assert!(diags.is_empty(), "{msgs:?}");
    }

    #[test]
    fn qualified_expression_in_concat_and_aggregate_form() {
        // `'0' & signed'(abs(b))` (Vivado std_logic_arith mult): the qualified
        // conversion is a plain operand of `&`; a qualified aggregate
        // `t'(field => v, others => x)` is a placeholder, not an error.
        let diags = lower_src("entity top is end entity;\narchitecture rtl of top is\n  signal b : bit_vector(7 downto 0) := (others => '0');\n  signal x : bit_vector(15 downto 0);\n  type rec_t is record elem : bit_vector(2 downto 0); end record;\n  signal r : rec_t;\nbegin\n  x <= '0' & bit_vector'(b);\n  r <= rec_t'(elem => \"000\");\n  r <= rec_t'(others => '0');\nend architecture;\n");
        assert_no_errors(&diags);
    }

    #[test]
    fn real_literal_with_digit_separators() {
        // math_real.vhdl: `constant MATH_E : real := 2.71828_18284_59045_23536;`
        let diags = lower_src("package p is\n  constant math_e : real := 2.71828_18284_59045_23536;\n  constant math_pi : real := 3.14159_26535_89793_23846;\nend package;\n");
        assert_no_errors(&diags);
    }

    #[test]
    fn package_level_signal_bare_read_via_use() {
        // `signal net : network_t` in a package; bare `net` in a using file.
        let mut db = SourceDb::new();
        let id1 = db.add_file("p.vhd".to_string(), "package p is\n  type network_t is record id : integer; end record;\n  signal net : network_t;\nend package;\n".to_string()).unwrap();
        let parsed1 = hdl_syntax::vhdl::parser::parse_file(id1, db.file(id1));
        let mut diags = Vec::new();
        let mut pkgs = HashMap::new();
        let lowered = lower_file(
            id1,
            &parsed1.root,
            &db,
            &mut diags,
            1_000_000,
            &HashMap::new(),
            &std::rc::Rc::new(HashMap::new()),
            &HashMap::new(),
        );
        for p in lowered.packages {
            pkgs.insert(p.name.to_ascii_lowercase(), std::rc::Rc::new(p));
        }
        let id2 = db.add_file("top.vhd".to_string(), "use work.p.all;\nentity top is port (q : out bit); end entity;\narchitecture rtl of top is\nbegin\n  q <= bit'val(net.id = 0);\nend architecture;\n".to_string()).unwrap();
        let parsed2 = hdl_syntax::vhdl::parser::parse_file(id2, db.file(id2));
        let _ = lower_file(
            id2,
            &parsed2.root,
            &db,
            &mut diags,
            1_000_000,
            &pkgs,
            &pkg_funcs_from(&pkgs),
            &HashMap::new(),
        );
        assert_no_errors(&diags);
    }

    #[test]
    fn invalid_generate_range_and_literal_base_do_not_panic() {
        // Regression: a for-generate over a real range (`0.0 to 8.0`) read the
        // f64 bit pattern as a huge u64 and overflowed the unroll Vec; a based
        // literal with base 0 (`0#121#`) panicked in from_str_radix.
        for src in [
            "entity e is end;\narchitecture a of e is\nbegin\n  g : for i in 0.0 to 8.0 generate\n    null;\n  end generate g;\nend;\n",
            "entity e is end;\narchitecture a of e is\nbegin\n  process\n    variable i : integer;\n  begin\n    i := 0#121#E2;\n    wait;\n  end process;\nend;\n",
        ] {
            let mut db = SourceDb::new();
            let id = db.add_file("t.vhd".to_string(), src.to_string()).unwrap();
            let parsed = hdl_syntax::vhdl::parser::parse_file(id, db.file(id));
            let mut diags = Vec::new();
            let _ = lower_file(
                id,
                &parsed.root,
                &db,
                &mut diags,
                1_000_000,
                &HashMap::new(),
                &std::rc::Rc::new(HashMap::new()),
                &HashMap::new(),
            );
        }
    }
}
