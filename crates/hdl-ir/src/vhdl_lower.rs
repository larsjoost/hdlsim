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
//!
//! Layout: this file holds the public IR types, the `Lower` context struct
//! (its fields are the module's real interface — each submodule's `//!`
//! header states which fields it owns), `lower_file`, and the tests. The
//! lowering itself lives in per-responsibility submodules: `context`-free
//! helpers in `scalar`, config/binding in `config`, architecture selection
//! in `variant`, object/record naming in `objects`, type decls in `types`,
//! widths in `width`, entities in `entity`, packages in `package`,
//! architectures/concurrent in `arch`, object decls in `decls`,
//! subprograms in `subprogram` (slot machinery in `slots`), processes in
//! `process`, sequential statements in `seq`, aggregates in `aggregate`,
//! and expressions/attributes in `expr` + `dims` + `dump`.

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

mod aggregate;
mod arch;
mod config;
mod decls;
mod dims;
mod dump;
mod entity;
mod expr;
mod objects;
mod package;
mod process;
mod scalar;
mod seq;
mod slots;
mod subprogram;
mod types;
mod variant;
mod width;
pub(crate) use scalar::*;
use variant::{apply_default_archs, arch_entity};
pub use variant::{lower_entity_variant, lower_pending_architectures, stem_key};

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

    // ---- declarations ------------------------------------------------------------

    // ---- processes and statements ---------------------------------------------------

    // ---- expressions ---------------------------------------------------------------
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
