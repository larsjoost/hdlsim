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
use crate::value::{BinOp, UnOp, Value, V0, V1, VDASH, VX, VZ};
use crate::verilog_lower::const_eval_f;
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

/// Lower one parsed VHDL file into modules (entities with their architectures
/// attached). `time_unit_fs` is the fallback for physical literals; VHDL
/// physical literals carry their own unit (`10 ns`), so this is only used for
/// unit-less numbers.
/// What lowering one VHDL file produced: modules plus the package units the
/// file declared (retained on the `Design` for other files' `use` clauses).
pub struct Lowered {
    pub modules: Vec<ModuleIr>,
    pub packages: Vec<PackageIr>,
    /// Architectures whose entity was not declared in the same file; bound
    /// design-wide later via [`lower_pending_architectures`].
    pub pending: Vec<PendingArch>,
}

/// An architecture deferred until its entity is known design-wide (the entity
/// may be declared in a file compiled later). Holds the file root so the
/// context clauses can be re-applied when the architecture is lowered.
#[derive(Clone, Debug)]
pub struct PendingArch {
    pub root: SyntaxNode,
    pub arch: SyntaxNode,
}

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
    /// The package's own declaration node and the design-file root that
    /// declared it — retained so a package instantiation can re-lower the
    /// generic package with the `generic map` values as overrides.
    pub decl: Option<SyntaxNode>,
    pub src: Option<SyntaxNode>,
}

/// Unit names referenced by the context clauses (`use lib.pkg...;`,
/// `context lib.pkg...;`) at the top of a design file — e.g.
/// `use ieee.std_logic_1164.all` yields `std_logic_1164`. The library manager
/// resolves these before lowering the file so the packages are visible.
pub fn context_units(db: &SourceDb, root: &SyntaxNode) -> Vec<String> {
    let mut out = Vec::new();
    for child in real_children(root) {
        if matches!(
            child.kind(),
            SyntaxKind::ContextClause | SyntaxKind::UseClause
        ) {
            for n in real_children(&child) {
                if n.kind() == SyntaxKind::Name {
                    let text = db.span_text(n.span()).trim();
                    let parts: Vec<&str> = text.split('.').collect();
                    // `lib.unit.all` / `lib.unit.item` / `unit.all` / `unit.item`
                    let unit = match parts.len() {
                        n if n >= 3 => parts[n - 2],
                        2 => parts[0],
                        _ => continue,
                    };
                    if !unit.is_empty() && !out.iter().any(|u| u == unit) {
                        out.push(unit.to_ascii_lowercase());
                    }
                }
            }
        }
    }
    out
}

pub fn lower_file(
    file: FileId,
    root: &SyntaxNode,
    db: &SourceDb,
    diags: &mut Vec<Diagnostic>,
    _time_unit_fs: u64,
    pkgs: &HashMap<String, PackageIr>,
) -> Lowered {
    let _ = file;
    let mut lx = Lower::new(db, diags);
    lx.register_builtin_types();
    lx.file_root = Some(root.clone());
    lx.pkgs = pkgs
        .iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), v.clone()))
        .collect();
    lx.apply_context_clauses(root);
    let mut modules: Vec<ModuleIr> = Vec::new();
    let mut entities: HashMap<String, usize> = HashMap::new();
    let mut packages: Vec<PackageIr> = Vec::new();
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
                    .or_insert_with(|| p.clone());
                packages.push(p);
            }
            SyntaxKind::PackageInst => {
                if let Some(p) = lx.lower_package_inst(&child) {
                    // The instantiation's items win over the generic
                    // package's defaults that pass 0 put into scope.
                    lx.merge_package(&p, None, true);
                    lx.pkgs.insert(p.name.to_ascii_lowercase(), p.clone());
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
    for child in real_children(root) {
        if child.kind() == SyntaxKind::Entity {
            let m = lx.lower_entity(&child);
            entities.insert(m.name.clone(), modules.len());
            modules.push(m);
        }
    }
    // Pass 2: configurations (default architecture bindings) and
    // architectures (bodies). An architecture whose entity is not declared in
    // this file is deferred (`pending`): the entity may live in a file
    // compiled later, so binding happens design-wide in
    // `lower_pending_architectures`.
    let mut last_arch: HashMap<String, String> = HashMap::new();
    let mut config_defaults: HashMap<String, String> = HashMap::new();
    for child in real_children(root) {
        match child.kind() {
            SyntaxKind::ConfigDecl => {
                if let Some((ent, arch)) = config_default(&child, db) {
                    config_defaults.insert(ent, arch);
                }
            }
            SyntaxKind::Architecture => {
                let known = arch_entity(&child, db)
                    .map(|(_, name)| entities.contains_key(&name))
                    .unwrap_or(false);
                if known {
                    if let Some((ent, arch)) =
                        lx.lower_architecture(&child, &mut modules, &entities)
                    {
                        last_arch.insert(ent, arch);
                    }
                } else {
                    pending.push(PendingArch {
                        root: root.clone(),
                        arch: child.clone(),
                    });
                }
            }
            _ => {}
        }
    }
    apply_default_archs(&mut modules, &entities, &last_arch, &config_defaults);
    // Package instantiations from declarative parts (registered on the fly).
    packages.extend(lx.inst_packages.drain(..));
    Lowered {
        modules,
        packages,
        pending,
    }
}

/// `configuration cfg of ENT is for ARCH ... end for;` → (entity, arch).
fn config_default(c: &SyntaxNode, db: &SourceDb) -> Option<(String, String)> {
    let kids = real_children(c);
    let ent = kids
        .iter()
        .skip_while(|k| db.span_text(k.span()).trim().to_ascii_lowercase() != "of")
        .nth(1)
        .filter(|k| k.kind() == SyntaxKind::Name)
        .map(|k| db.span_text(k.span()).trim().to_string())?;
    let ent = ent.rsplit('.').next().unwrap_or("").to_string();
    let fs = kids.iter().find(|k| k.kind() == SyntaxKind::ForSpec)?;
    let arch = real_children(fs)
        .iter()
        .filter(|k| k.kind() == SyntaxKind::Ident)
        .map(|k| db.span_text(k.span()).trim().to_ascii_lowercase())
        .find(|w| {
            !matches!(
                w.as_str(),
                "for" | "end" | "use" | "entity" | "configuration"
            )
        })?;
    Some((ent, arch))
}

/// Give each plain entity module the body of its default architecture:
/// the one named by a configuration declaration if any, else the last one
/// analyzed (IEEE default binding rule). Bodies live in `entity/arch`
/// variant modules produced by `lower_architecture`.
fn apply_default_archs(
    modules: &mut Vec<ModuleIr>,
    entities: &HashMap<String, usize>,
    last_arch: &HashMap<String, String>,
    config_defaults: &HashMap<String, String>,
) {
    for (ent, &base) in entities {
        let Some(ch) = config_defaults.get(ent).or_else(|| last_arch.get(ent)) else {
            continue;
        };
        let vname = format!("{ent}/{ch}");
        if let Some(vpos) = modules.iter().position(|m| m.name == vname) {
            let mut v = modules[vpos].clone();
            v.name = ent.clone();
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
pub fn lower_pending_architectures(
    db: &SourceDb,
    diags: &mut Vec<Diagnostic>,
    pkgs: &HashMap<String, PackageIr>,
    modules: &mut Vec<ModuleIr>,
    pending: Vec<PendingArch>,
    emit_errors: bool,
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
    for p in pending {
        let mut lx = Lower::new(db, diags);
        lx.register_builtin_types();
        lx.file_root = Some(p.root.clone());
        lx.pkgs = pkgs
            .iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), v.clone()))
            .collect();
        lx.apply_context_clauses(&p.root);
        match lx.lower_architecture(&p.arch, modules, &entities) {
            Some((ent, arch)) => {
                last_arch.insert(ent, arch);
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
    apply_default_archs(modules, &entities, &last_arch, &HashMap::new());
    still_pending
}

/// The entity an architecture belongs to (`architecture NAME of ENTITY is ...`)
/// → (entity ident node, entity name).
fn arch_entity(a: &SyntaxNode, db: &SourceDb) -> Option<(SyntaxNode, String)> {
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

/// Result of lowering a concurrent signal assignment: either a continuous
/// assign or (for delayed waveforms) a one-shot process.
pub(crate) enum ConcOut {
    Assign(AssignIr),
    Proc(ProcessIr),
}

pub(crate) fn real_children(n: &SyntaxNode) -> Vec<SyntaxNode> {
    n.children()
        .filter(|c| !matches!(c.kind(), SyntaxKind::Whitespace | SyntaxKind::Comment))
        .collect()
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

fn is_expr_node(k: SyntaxKind) -> bool {
    matches!(
        k,
        SyntaxKind::Literal
            | SyntaxKind::Name
            | SyntaxKind::ParenExpr
            | SyntaxKind::BinExpr
            | SyntaxKind::UnaryExpr
            | SyntaxKind::Call
            | SyntaxKind::Concat
            | SyntaxKind::Aggregate
            | SyntaxKind::PartSelect
            | SyntaxKind::Index
            | SyntaxKind::Attr
            | SyntaxKind::Others
    )
}

/// True for node kinds produced by `parse_decl` — used to tell a generate or
/// block declarative item from a concurrent statement when no `begin`
/// keyword separates them.
fn is_decl_kind(k: SyntaxKind) -> bool {
    matches!(
        k,
        SyntaxKind::SignalDecl
            | SyntaxKind::ConstantDecl
            | SyntaxKind::VariableDecl
            | SyntaxKind::ComponentDecl
            | SyntaxKind::TypeDecl
            | SyntaxKind::SubtypeDecl
            | SyntaxKind::Function
            | SyntaxKind::AliasDecl
            | SyntaxKind::AttrDecl
            | SyntaxKind::AttrSpec
            | SyntaxKind::FileDecl
            | SyntaxKind::UseClause
            | SyntaxKind::ForSpec
            | SyntaxKind::PackageInst
    )
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
}

struct Lower<'a> {
    db: &'a SourceDb,
    diags: &'a mut Vec<Diagnostic>,
    /// Compile-time constants in scope (enum literals, constants, generics).
    consts: HashMap<String, Value>,
    /// Constant name -> type mark, so record/array-typed constants can be
    /// materialized as hidden signals for runtime indexing (`c(i).field`).
    const_types: HashMap<String, String>,
    /// User function bodies in scope (package functions via `use`), for
    /// const-evaluating calls in constant initializers.
    funcs: HashMap<String, FuncIr>,
    /// Type marks in scope (built-ins + user type/subtype decls).
    types: HashMap<String, TypeInfo>,
    /// Subtype name -> constrained width, so `signal x : my_subtype` (no
    /// constraint at the use site) keeps the subtype's width.
    type_widths: HashMap<String, usize>,
    /// Signal/variable name → signal id in the current module.
    names: HashMap<String, SigId>,
    /// Signal id → memory depth (for `name'range` loops over arrays).
    mems: HashMap<SigId, usize>,
    /// Signal id → signed flag (for `resize`/`to_signed` context).
    sig_signed: HashMap<SigId, bool>,
    /// Signal id → record type name (for `sig.field` selects), if record-typed.
    sig_rec: HashMap<SigId, String>,
    /// Signal id → element record type name for arrays of records, so
    /// `arr(i).field` lowers to a slice of the memory element.
    sig_elem_rec: HashMap<SigId, String>,
    /// Signal id → width in bits (for `'length`/`'high`/… attributes).
    sig_width: HashMap<SigId, usize>,
    /// Signal id → array dimensions [(low, size), …] (for `m(i, j)` flat index).
    sig_dims: HashMap<SigId, Vec<(i64, usize)>>,
    /// Hidden signals allocated while lowering statements (for-loop vars),
    /// appended to the module after the process body is lowered.
    extra_signals: Vec<SignalIr>,
    /// `module.signals.len()` captured when statement lowering starts.
    extra_base: usize,
    /// Counter for auto-naming instances (the parser drops statement labels,
    /// so `InstIr.name` is always empty; elab needs unique prefixes).
    inst_count: usize,
    /// Ports declared without a constraint (`port (a : in std_ulogic_vector)`)
    /// — their width is only known after port connection, so static
    /// attributes on them lower to runtime `Expr::SigAttr`.
    uncon_ports: std::collections::HashSet<SigId>,
    /// `alias name is <expr>;` — name → lowered expression (slice/index
    /// aliases can't be a plain SigId). Whole-signal aliases are additionally
    /// inserted into `names`.
    aliases: HashMap<String, Expr>,
    /// All packages visible while lowering this file (library packages from
    /// other files plus same-file ones), keyed by lowercased unit name —
    /// backs `use lib.pkg...;` inside declarative parts and package
    /// instantiations.
    pkgs: HashMap<String, PackageIr>,
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
}

/// Per-module signal metadata (`SigId` is relative to one module).
#[derive(Default, Clone)]
struct SigMaps {
    mems: HashMap<SigId, usize>,
    sig_signed: HashMap<SigId, bool>,
    sig_rec: HashMap<SigId, String>,
    sig_elem_rec: HashMap<SigId, String>,
    sig_width: HashMap<SigId, usize>,
    sig_dims: HashMap<SigId, Vec<(i64, usize)>>,
    uncon_ports: std::collections::HashSet<SigId>,
}

impl<'a> Lower<'a> {
    fn new(db: &'a SourceDb, diags: &'a mut Vec<Diagnostic>) -> Lower<'a> {
        Lower {
            db,
            diags,
            consts: HashMap::new(),
            const_types: HashMap::new(),
            funcs: HashMap::new(),
            types: HashMap::new(),
            type_widths: HashMap::new(),
            names: HashMap::new(),
            mems: HashMap::new(),
            sig_signed: HashMap::new(),
            sig_rec: HashMap::new(),
            sig_elem_rec: HashMap::new(),
            sig_width: HashMap::new(),
            sig_dims: HashMap::new(),
            extra_signals: Vec::new(),
            extra_base: 0,
            inst_count: 0,
            uncon_ports: std::collections::HashSet::new(),
            aliases: HashMap::new(),
            pkgs: HashMap::new(),
            file_root: None,
            inst_packages: Vec::new(),
            entity_maps: HashMap::new(),
        }
    }

    /// Snapshot the current signal maps (called after an entity's ports are
    /// lowered) and clear them so the next entity starts fresh.
    fn snapshot_entity_maps(&mut self, entity: &str) {
        self.entity_maps.insert(
            entity.to_string(),
            SigMaps {
                mems: self.mems.clone(),
                sig_signed: self.sig_signed.clone(),
                sig_rec: self.sig_rec.clone(),
                sig_elem_rec: self.sig_elem_rec.clone(),
                sig_width: self.sig_width.clone(),
                sig_dims: self.sig_dims.clone(),
                uncon_ports: self.uncon_ports.clone(),
            },
        );
        self.mems.clear();
        self.sig_signed.clear();
        self.sig_rec.clear();
        self.sig_elem_rec.clear();
        self.sig_width.clear();
        self.sig_dims.clear();
        self.uncon_ports.clear();
    }

    /// Install the signal maps of the entity whose architecture is being
    /// lowered (SigId is module-relative).
    fn install_entity_maps(&mut self, entity: &str) {
        let m = self.entity_maps.get(entity).cloned().unwrap_or_default();
        self.mems = m.mems;
        self.sig_signed = m.sig_signed;
        self.sig_rec = m.sig_rec;
        self.sig_elem_rec = m.sig_elem_rec;
        self.sig_width = m.sig_width;
        self.sig_dims = m.sig_dims;
        self.uncon_ports = m.uncon_ports;
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
    }

    /// Merge one `use lib.pkg.item;` / `context lib.pkg;` clause into scope.
    fn merge_use_clause(&mut self, u: &SyntaxNode) {
        for n in real_children(u) {
            if n.kind() == SyntaxKind::Name {
                let text = self.db.span_text(n.span()).trim();
                let parts: Vec<&str> = text.split('.').collect();
                let (unit, item) = match parts.len() {
                    n if n >= 3 => (parts[n - 2], parts[n - 1]),
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
        self.diags.push(Diagnostic {
            code: "E0401",
            severity: Severity::Error,
            span: Some(span),
            message: msg.into(),
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

    fn register_builtin_types(&mut self) {
        self.consts.insert("true".into(), Value { bits: vec![V1] });
        self.consts.insert("false".into(), Value { bits: vec![V0] });
        // severity_level literals (std.standard) — passed to vital_*/assert
        // helper procedures as plain values, e.g. `MsgSeverity => ERROR`.
        self.consts.insert("note".into(), Value::from_u64(32, 0));
        self.consts.insert("warning".into(), Value::from_u64(32, 1));
        self.consts.insert("error".into(), Value::from_u64(32, 2));
        self.consts.insert("failure".into(), Value::from_u64(32, 3));
        // std.textio `justified` literals (the `write(l, v, left, 13)` form).
        self.consts.insert("left".into(), Value::from_u64(32, 0));
        self.consts.insert("right".into(), Value::from_u64(32, 1));
        self.types.insert("std_logic".into(), TypeInfo::Scalar);
        // std_ulogic is the enum from ieee.std_logic_1164; hdlsim models it as
        // a 1-bit scalar like std_logic (the `use` clause is not needed).
        self.types.insert("std_ulogic".into(), TypeInfo::Scalar);
        self.types.insert("bit".into(), TypeInfo::Scalar);
        self.types.insert("boolean".into(), TypeInfo::Scalar);
        self.types.insert(
            "std_logic_vector".into(),
            TypeInfo::Vector { signed: false },
        );
        self.types.insert(
            "std_ulogic_vector".into(),
            TypeInfo::Vector { signed: false },
        );
        self.types
            .insert("bit_vector".into(), TypeInfo::Vector { signed: false });
        self.types
            .insert("unsigned".into(), TypeInfo::Vector { signed: false });
        self.types
            .insert("signed".into(), TypeInfo::Vector { signed: true });
        self.types
            .insert("integer".into(), TypeInfo::Integer { signed: true });
        self.types.insert("real".into(), TypeInfo::Real);
        self.types
            .insert("natural".into(), TypeInfo::Integer { signed: false });
        self.types
            .insert("positive".into(), TypeInfo::Integer { signed: false });
        self.types.insert("char".into(), TypeInfo::Char);
        self.types.insert("character".into(), TypeInfo::Char);
        self.types
            .insert("string".into(), TypeInfo::Vector { signed: false });
        // std.textio: `line` is a string buffer (simulated as a wide vector,
        // see resolve_type); `text` is a file object (its name is bound to the
        // file path by lower_file_decl, not a signal).
        self.types
            .insert("line".into(), TypeInfo::Vector { signed: false });
        self.types
            .insert("text".into(), TypeInfo::Vector { signed: false });
        self.types
            .insert("time".into(), TypeInfo::Integer { signed: false });
    }

    /// Resolve a type mark + optional constraint to (width, signed, mem_depth).
    fn resolve_type(
        &mut self,
        mark: &SyntaxNode,
        constraint: Option<&SyntaxNode>,
        span: Span,
    ) -> (usize, bool, Option<usize>, bool) {
        let name = self.kw(mark);
        let base = self.types.get(&name).cloned();
        let Some(base) = base else {
            self.diags.push(Diagnostic {
                code: "E0401",
                severity: Severity::Error,
                span: Some(span),
                message: format!("unknown type '{}'", name),
                help: Some(
                    "known type marks: bit, boolean, character, integer, natural, positive, \
                     real, time, string, bit_vector, std_logic, std_ulogic, std_logic_vector, \
                     std_ulogic_vector, unsigned, signed \
                     (std.standard / ieee.std_logic_1164 / ieee.numeric_std)"
                        .to_string(),
                ),
            });
            return (1, false, None, false);
        };
        match base {
            TypeInfo::Vector { signed } => {
                // An unconstrained `line` (std.textio) is a string buffer of
                // up to TEXTIO_LINE_CAP bytes.
                if name == "line" && constraint.is_none() {
                    return (TEXTIO_LINE_CAP * 8, signed, None, false);
                }
                let w = constraint
                    .map(|c| self.constraint_width(c))
                    .unwrap_or_else(|| self.type_widths.get(&name).copied().unwrap_or(1))
                    .max(1);
                // Strings pack 8 bits per character (like string literals).
                let w = if name == "string" { w * 8 } else { w };
                (w, signed, None, false)
            }
            TypeInfo::Integer { signed } => (32, signed, None, false),
            TypeInfo::Real => (64, false, None, true),
            TypeInfo::Scalar => (1, false, None, false),
            TypeInfo::Char => (8, false, None, false),
            TypeInfo::Enum(ref lits) => {
                let w = bits_for(lits.len());
                (w, false, None, false)
            }
            TypeInfo::Array { elem, depth, .. } => (elem, false, Some(depth), false),
            TypeInfo::Record { fields } => (
                fields.iter().map(|f| f.1).sum::<usize>().max(1),
                false,
                None,
                false,
            ),
        }
    }

    /// Width from a constraint: `(msb downto lsb)` / `(msb to lsb)` / `(n)`,
    /// or multi-dimensional `(r1, r2, ...)` → product of the dimension widths.
    fn constraint_width(&mut self, c: &SyntaxNode) -> usize {
        let kids = real_children(c);
        let subs: Vec<SyntaxNode> = kids
            .into_iter()
            .filter(|k| k.kind() == SyntaxKind::Range)
            .collect();
        if !subs.is_empty() {
            return subs
                .iter()
                .map(|s| self.range_width(s))
                .fold(1usize, |a, b| a * b.max(1))
                .max(1);
        }
        self.range_width(c)
    }

    /// Width of a single range: `a to/downto b`, `(n)`, box `<>`, or an
    /// index given as a type mark (`array (enum_t) of ...` → literal count).
    fn range_width(&mut self, r: &SyntaxNode) -> usize {
        // Box constraint `type range <>` — genuinely unconstrained; do not try
        // to evaluate the index type name (it would report "unknown identifier").
        if self.text(r).contains("<>") {
            return 1;
        }
        let kids = real_children(r);
        // Index given as a type mark: the depth is the enum's literal count.
        // A name that is a *constant* (not a type) still goes through the
        // expression path below.
        if let Some(n) = kids.iter().find(|k| k.kind() == SyntaxKind::Name) {
            let tn = self.text(n).to_ascii_lowercase();
            match self.types.get(&tn) {
                Some(TypeInfo::Enum(lits)) => return lits.len().max(1),
                Some(_) => return 1,
                None => {}
            }
        }
        let exprs: Vec<Expr> = kids
            .iter()
            .filter(|k| is_expr_node(k.kind()))
            .map(|k| self.lower_expr(k, &HashMap::new()))
            .collect();
        if exprs.len() >= 2 {
            if let (Some(a), Some(b)) = (
                self.ce_env(&exprs[0], &HashMap::new()),
                self.ce_env(&exprs[1], &HashMap::new()),
            ) {
                if let (Some(a), Some(b)) = (a.to_u64(), b.to_u64()) {
                    return (a as i64 - b as i64).unsigned_abs() as usize + 1;
                }
            }
            1
        } else if exprs.len() == 1 {
            self.ce_env(&exprs[0], &HashMap::new())
                .and_then(|v| v.to_u64())
                .map(|n| n as usize)
                .unwrap_or(1)
        } else {
            1
        }
    }

    /// Dimensions of an array constraint: [(low, size), …] per index range.
    fn constraint_dims(&mut self, c: &SyntaxNode) -> Vec<(i64, usize)> {
        let kids = real_children(c);
        let subs: Vec<SyntaxNode> = kids
            .into_iter()
            .filter(|k| k.kind() == SyntaxKind::Range)
            .collect();
        let ranges: Vec<SyntaxNode> = if subs.is_empty() {
            vec![c.clone()]
        } else {
            subs
        };
        let mut dims = Vec::new();
        for r in ranges {
            if self.text(&r).contains("<>") {
                dims.push((0, 1));
                continue;
            }
            let rk = real_children(&r);
            if let Some(n) = rk.iter().find(|k| k.kind() == SyntaxKind::Name) {
                let tn = self.text(n).to_ascii_lowercase();
                match self.types.get(&tn) {
                    Some(TypeInfo::Enum(lits)) => {
                        dims.push((0, lits.len()));
                        continue;
                    }
                    Some(_) => {
                        dims.push((0, 1));
                        continue;
                    }
                    None => {}
                }
            }
            let exprs: Vec<Expr> = rk
                .iter()
                .filter(|k| is_expr_node(k.kind()))
                .map(|k| self.lower_expr(k, &HashMap::new()))
                .collect();
            if exprs.len() >= 2 {
                let a = self
                    .ce_env(&exprs[0], &HashMap::new())
                    .and_then(|v| v.to_u64());
                let b = self
                    .ce_env(&exprs[1], &HashMap::new())
                    .and_then(|v| v.to_u64());
                if let (Some(a), Some(b)) = (a, b) {
                    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
                    dims.push((lo as i64, (hi - lo + 1) as usize));
                } else {
                    dims.push((0, 1));
                }
            } else if exprs.len() == 1 {
                let n = self
                    .ce_env(&exprs[0], &HashMap::new())
                    .and_then(|v| v.to_u64())
                    .unwrap_or(1);
                dims.push((0, n as usize));
            } else {
                dims.push((0, 1));
            }
        }
        dims
    }

    /// Materialize a record/array-typed constant as a hidden signal holding
    /// its value, so runtime selection works: `c(i)`, `c(i).field`, `c.field`
    /// on a constant lower to Idx/Sel over `__const_<name>`.
    fn materialize_const(&mut self, module: &mut ModuleIr, d: &SyntaxNode) {
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
            .map(|k| self.text(k).to_ascii_lowercase())
            .collect();
        for n in names {
            let hidden = format!("__const_{}", n);
            if self.names.contains_key(&hidden) {
                continue;
            }
            let Some(tn) = self.const_types.get(&n).cloned() else {
                continue;
            };
            let Some(ti) = self.types.get(&tn).cloned() else {
                continue;
            };
            let Some(val) = self.consts.get(&n).cloned() else {
                continue;
            };
            match ti {
                TypeInfo::Array {
                    elem,
                    depth,
                    dims,
                    elem_name,
                } => {
                    let id = module.signals.len() as SigId;
                    module.signals.push(SignalIr {
                        name: hidden.clone(),
                        width: elem.max(1),
                        kind: SigKind::Wire,
                        init: Some(val),
                        mem: Some(depth.max(1)),
                        signed: false,
                    });
                    self.names.insert(hidden, id);
                    self.mems.insert(id, depth.max(1));
                    self.sig_width.insert(id, elem.max(1));
                    if !dims.is_empty() {
                        self.sig_dims.insert(id, dims);
                    }
                    if let Some(en) = elem_name {
                        if matches!(self.types.get(&en), Some(TypeInfo::Record { .. })) {
                            self.sig_elem_rec.insert(id, en);
                        }
                    }
                }
                TypeInfo::Record { fields } => {
                    let width = fields.iter().map(|f| f.1).sum::<usize>().max(1);
                    let id = module.signals.len() as SigId;
                    module.signals.push(SignalIr {
                        name: hidden.clone(),
                        width,
                        kind: SigKind::Wire,
                        init: Some(val),
                        mem: None,
                        signed: false,
                    });
                    self.names.insert(hidden, id);
                    self.sig_width.insert(id, width);
                    self.sig_rec.insert(id, tn);
                }
                _ => {}
            }
        }
    }

    /// `c(i)` where `c` is an array-typed constant (e.g. a function-local
    /// constant, which cannot be a module signal because function bodies lower
    /// in slot space): lower to a `$const_idx` system call over the literal
    /// value — args are [flat value, flat index, element width].
    fn lower_const_array_index(&mut self, fname: &str, args: &[Expr]) -> Option<Expr> {
        let tn = self.const_types.get(fname)?;
        let TypeInfo::Array { elem, dims, .. } = self.types.get(tn)? else {
            return None;
        };
        let val = self.consts.get(fname)?.clone();
        let flat = if !dims.is_empty() && dims.len() == args.len() {
            let mut flat: Option<Expr> = None;
            for (arg, (lo, n)) in args.iter().zip(dims.iter()) {
                let a = Expr::Bin {
                    op: BinOp::Sub,
                    l: Box::new(arg.clone()),
                    r: Box::new(Expr::Lit(Value::from_u64(32, *lo as u64))),
                };
                flat = Some(match flat {
                    None => a,
                    Some(f) => Expr::Bin {
                        op: BinOp::Add,
                        l: Box::new(Expr::Bin {
                            op: BinOp::Mul,
                            l: Box::new(f),
                            r: Box::new(Expr::Lit(Value::from_u64(32, *n as u64))),
                        }),
                        r: Box::new(a),
                    },
                });
            }
            flat?
        } else {
            args.first()?.clone()
        };
        Some(Expr::Call {
            name: "$const_idx".into(),
            args: vec![
                Expr::Lit(val),
                flat,
                Expr::Lit(Value::from_u64(32, *elem as u64)),
            ],
        })
    }

    fn sig_id(&self, name: &str) -> Option<SigId> {
        self.names.get(name).copied()
    }

    /// Const-eval in the lowerer's constant scope, with visible package
    /// function bodies (for calls in constant initializers).
    fn ce(&self, e: &Expr) -> Option<Value> {
        const_eval_f(e, &self.consts, &self.funcs)
    }

    /// Const-eval against an explicit env (loop vars, generics) plus package
    /// function bodies.
    fn ce_env(&self, e: &Expr, env: &HashMap<String, Value>) -> Option<Value> {
        const_eval_f(e, env, &self.funcs)
    }

    // ---- entities / architectures ---------------------------------------------

    fn lower_entity(&mut self, e: &SyntaxNode) -> ModuleIr {
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
        };
        self.names.clear();
        for k in &kids {
            match k.kind() {
                SyntaxKind::GenericClause => self.lower_generic_clause(&mut module, k),
                SyntaxKind::PortClause => self.lower_port_clause(&mut module, k),
                _ => {}
            }
        }
        // Rebuild names from signals (ports registered above).
        self.names = module
            .signals
            .iter()
            .enumerate()
            .map(|(i, s)| (s.name.clone(), i as SigId))
            .collect();
        self.snapshot_entity_maps(&module.name);
        module
    }

    fn lower_generic_clause(&mut self, module: &mut ModuleIr, g: &SyntaxNode) {
        for (name, def) in self.lower_generic_decls(g, &HashMap::new()) {
            module.params.push(ParamIr { name, default: def });
        }
    }

    /// Lower the `GenericDecl`s of a generic clause into `consts` (so later
    /// declarations see the generic names) and return `(name, value)` per
    /// generic. `overrides` (from a `generic map` at a package instantiation)
    /// replaces the declared default values.
    fn lower_generic_decls(
        &mut self,
        g: &SyntaxNode,
        overrides: &HashMap<String, Value>,
    ) -> Vec<(String, Value)> {
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
            let type_mark = after.iter().find(|k| k.kind() == SyntaxKind::Name);
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
            let span = d.span();
            let (width, signed, _, _) = match type_mark {
                Some(tm) => self.resolve_type(tm, constraint, span),
                None => (32, false, None, false),
            };
            let def = default
                .as_ref()
                .map(|e| {
                    let ex = self.lower_expr(e, &HashMap::new());
                    self.ce_env(&ex, &HashMap::new())
                })
                .flatten()
                .unwrap_or(Value::x(width.max(1)));
            let _ = (width, signed);
            for n in &names {
                let key = n.to_ascii_lowercase();
                let val = overrides.get(&key).cloned().unwrap_or_else(|| def.clone());
                self.consts.insert(key, val.clone());
                out.push((n.clone(), val));
            }
        }
        out
    }

    fn lower_port_clause(&mut self, module: &mut ModuleIr, p: &SyntaxNode) {
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
            // The type mark is the last Name child (after the ':' punct).
            if let Some(pos) = dk
                .iter()
                .position(|k| k.kind() == SyntaxKind::Punct && self.text(k) == ":")
            {
                for k in &dk[pos + 1..] {
                    if k.kind() == SyntaxKind::Name && type_mark.is_none() {
                        type_mark = Some(k.clone());
                    }
                }
                // Port names are Name children before the ':'.
                names = dk[..pos]
                    .iter()
                    .filter(|k| k.kind() == SyntaxKind::Name)
                    .map(|k| self.text(k))
                    .collect();
                // ...but the mode keyword was also before ':' and is an Ident, not a Name.
            }
            let span = d.span();
            let (width, signed, mem, _real) = match &type_mark {
                Some(tm) => self.resolve_type(tm, constraint.as_ref(), span),
                None => (1, false, None, false),
            };
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
                    init: None,
                    mem,
                    signed,
                });
                if let Some(d) = mem {
                    self.mems.insert(id, d);
                }
                self.sig_signed.insert(id, signed);
                self.sig_width.insert(id, width);
                if let Some(tm) = &type_mark {
                    let tn = self.text(tm).to_ascii_lowercase();
                    match self.types.get(&tn) {
                        Some(TypeInfo::Record { .. }) => {
                            self.sig_rec.insert(id, tn);
                        }
                        Some(TypeInfo::Array {
                            dims, elem_name, ..
                        }) => {
                            if let Some(en) = elem_name {
                                if matches!(self.types.get(en), Some(TypeInfo::Record { .. })) {
                                    self.sig_elem_rec.insert(id, en.clone());
                                }
                            }
                            if !dims.is_empty() {
                                let d = match &constraint {
                                    Some(c) => self.constraint_dims(c),
                                    None => dims.clone(),
                                };
                                self.sig_dims.insert(id, d);
                            }
                        }
                        _ => {}
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

    /// Merge a package's items into scope: all of them, or only `item`.
    /// Built-in type marks win (`or_insert`), so e.g. `std_logic` keeps the
    /// 1-bit scalar model even though ieee.std_logic_1164 declares it as a
    /// resolved 9-state subtype.
    /// With `force`, an instantiated package's items override existing ones
    /// (e.g. the generic package's own defaults that leaked into scope while
    /// it was lowered); otherwise built-ins and earlier bindings win.
    fn merge_package(&mut self, p: &PackageIr, item: Option<&str>, force: bool) {
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
            if item.is_none_or(|i| i == cname) {
                if force {
                    self.consts.insert(cname.clone(), cval.clone());
                } else {
                    self.consts
                        .entry(cname.clone())
                        .or_insert_with(|| cval.clone());
                }
            }
        }
        for f in &p.functions {
            if item.is_none_or(|i| i == &f.name.to_ascii_lowercase()) {
                // Last wins: the package body (with real bodies) follows the
                // declaration (signature-only) in the merged package.
                self.funcs.insert(f.name.to_ascii_lowercase(), f.clone());
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
            self.pkgs.insert(key.clone(), q.clone());
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
        // Package declarations: constants, types, and function bodies become
        // visible in this file.
        let types_before: std::collections::HashSet<String> = self.types.keys().cloned().collect();
        let consts_before: std::collections::HashSet<String> =
            self.consts.keys().cloned().collect();
        // VHDL-2008 generic clause first, so constants/types declared after it
        // see the generic values (`generic map` overrides win over defaults).
        let mut generics: Vec<(String, Value)> = Vec::new();
        for d in real_children(p) {
            if d.kind() == SyntaxKind::GenericClause {
                for (n, v) in self.lower_generic_decls(&d, overrides) {
                    generics.push((n.to_ascii_lowercase(), v));
                }
            }
        }
        let mut functions: Vec<FuncIr> = Vec::new();
        for d in real_children(p) {
            match d.kind() {
                SyntaxKind::ConstantDecl => self.lower_constant_decl(&d),
                SyntaxKind::TypeDecl => self.lower_type_decl(&d),
                SyntaxKind::SubtypeDecl => self.lower_subtype_decl(&d),
                SyntaxKind::AliasDecl => self.lower_alias_decl(&d),
                SyntaxKind::UseClause => self.merge_use_clause(&d),
                SyntaxKind::PackageInst => self.register_package_inst(&d),
                SyntaxKind::FileDecl => self.lower_file_decl(&d),
                SyntaxKind::Function => {
                    if self.function_locals_use_params(&d) {
                        // e.g. `variable v : std_ulogic_vector(n-1 downto 0);`
                        // — the width depends on a call argument, which is not
                        // known at lower time; skip registering the function.
                        let name = real_children(&d)
                            .iter()
                            .find(|k| k.kind() == SyntaxKind::Name)
                            .map(|k| self.text(k))
                            .unwrap_or_default();
                        self.warn(
                            d.span(),
                            format!(
                                "function '{name}' has parameter-dependent local declarations; not registered for constant evaluation"
                            ),
                        );
                    } else if let Some(f) = self.lower_function_node(&d) {
                        self.funcs.insert(f.name.to_ascii_lowercase(), f.clone());
                        functions.push(f);
                    }
                }
                _ => {}
            }
        }
        // Collect the items this package added, for cross-file `use` clauses.
        let types = self
            .types
            .iter()
            .filter(|(k, _)| !types_before.contains(*k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        // Single-character consts are char-type enum literals ('U', '0', ...);
        // they are never referenced as bare names (char literals lex as
        // CharLiteral) and merging them would shadow same-named signals (e.g. a
        // signal `u` vs std_ulogic's 'U'), so keep them file-local.
        let consts = self
            .consts
            .iter()
            .filter(|(k, _)| !consts_before.contains(*k) && k.chars().count() > 1)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        PackageIr {
            name,
            types,
            consts,
            functions,
            generics,
            decl: Some(p.clone()),
            src: Some(root.clone()),
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
            let mut p = tp.clone();
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
            let mut p = tp.clone();
            p.name = inst_name;
            return Some(p);
        }
        // Fresh scope: builtins + this file's and the generic package's own
        // context clauses + all known packages.
        let file_root = self.file_root.clone();
        let mut lx2 = Lower::new(self.db, self.diags);
        lx2.register_builtin_types();
        lx2.pkgs = self.pkgs.clone();
        if let Some(r) = &file_root {
            lx2.apply_context_clauses(r);
        }
        lx2.apply_context_clauses(&src_root);
        // `generic map` associations: PortConn children with a leading Name.
        let mut overrides: HashMap<String, Value> = HashMap::new();
        for k in real_children(node) {
            if k.kind() != SyntaxKind::PortConn {
                continue;
            }
            let ck = real_children(&k);
            let name_pos = ck.iter().position(|c| c.kind() == SyntaxKind::Name);
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
            if pkg.generics.is_empty() {
                pkg.generics = part.generics;
            }
        }
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
        let Some(&midx) = entities.get(&entity_name) else {
            return None;
        };
        let idents: Vec<&SyntaxNode> = kids
            .iter()
            .filter(|k| k.kind() == SyntaxKind::Ident)
            .collect();
        let arch_name = idents
            .get(1)
            .map(|k| self.text(k).to_ascii_lowercase())
            .unwrap_or_default();
        let mut module_owned = modules[midx].clone();
        module_owned.name = format!("{entity_name}/{arch_name}");
        let module = &mut module_owned;
        self.names = module
            .signals
            .iter()
            .enumerate()
            .map(|(i, s)| (s.name.clone(), i as SigId))
            .collect();
        self.install_entity_maps(&entity_name);
        // Declarative part: everything before `begin`.
        let begin_pos = kids
            .iter()
            .position(|k| k.kind() == SyntaxKind::Ident && self.kw(k) == "begin");
        let decls: Vec<&SyntaxNode> = match begin_pos {
            Some(b) => kids[..b].iter().collect(),
            None => kids.iter().collect(),
        };
        self.aliases.clear();
        for d in &decls {
            match d.kind() {
                SyntaxKind::SignalDecl => self.lower_signal_decl(module, d),
                SyntaxKind::ConstantDecl => {
                    self.lower_constant_decl(d);
                    self.materialize_const(module, d);
                }
                SyntaxKind::VariableDecl => self.lower_signal_decl(module, d), // rare at arch level
                SyntaxKind::TypeDecl => self.lower_type_decl(d),
                SyntaxKind::SubtypeDecl => self.lower_subtype_decl(d),
                SyntaxKind::ComponentDecl => {} // components resolve to entities by name
                SyntaxKind::Function => self.lower_function_decl(module, d),
                SyntaxKind::AliasDecl => self.lower_alias_decl(d),
                SyntaxKind::UseClause => self.merge_use_clause(d),
                SyntaxKind::PackageInst => self.register_package_inst(d),
                SyntaxKind::FileDecl => self.lower_file_decl(d),
                _ => {}
            }
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
        modules.push(module_owned);
        Some((entity_name, arch_name))
    }

    /// Lower one concurrent statement (architecture body, block, or generate
    /// body — generate unrolls and re-enters here per iteration/branch).
    fn lower_conc_stmt(&mut self, module: &mut ModuleIr, s: &SyntaxNode, proc_idx: &mut usize) {
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
                SyntaxKind::AliasDecl => self.lower_alias_decl(d),
                SyntaxKind::UseClause => self.merge_use_clause(d),
                SyntaxKind::PackageInst => self.register_package_inst(d),
                SyntaxKind::FileDecl => self.lower_file_decl(d),
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
        let env = self.consts.clone();
        let exprs: Vec<Expr> = kids
            .iter()
            .enumerate()
            .filter(|(i, k)| *i > in_pos && *i < gen_pos && is_expr_node(k.kind()))
            .map(|(_, k)| self.lower_expr(k, &env))
            .collect();
        if exprs.len() < 2 {
            return;
        }
        let ev = |e: &Expr| self.ce_env(e, &env).and_then(|v| v.to_u64());
        let (Some(lo), Some(hi)) = (ev(&exprs[0]), ev(&exprs[1])) else {
            self.warn(s.span(), "generate range is not constant; skipped");
            return;
        };
        let (decls, body) = self.gen_body(&kids[gen_pos + 1..]);
        let saved = var.as_ref().and_then(|v| self.consts.get(v).cloned());
        let vals: Vec<u64> = if downto {
            (hi..=lo).rev().collect()
        } else {
            (lo..=hi).collect()
        };
        for v in vals {
            if let Some(var) = &var {
                self.consts.insert(var.clone(), Value::from_u64(32, v));
            }
            self.lower_gen_decls(module, &decls, true);
            for k in &body {
                self.lower_conc_stmt(module, k, proc_idx);
            }
        }
        match (var, saved) {
            (Some(v), Some(old)) => {
                self.consts.insert(v, old);
            }
            (Some(v), None) => {
                self.consts.remove(&v);
            }
            _ => {}
        }
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
                self.lower_gen_decls(module, &seg.decls, false);
                for st in &seg.stmts {
                    self.lower_conc_stmt(module, st, proc_idx);
                }
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
                self.lower_gen_decls(module, &decls, false);
                for st in &stmts {
                    self.lower_conc_stmt(module, st, proc_idx);
                }
                return;
            }
            if has_others {
                others = Some((decls, stmts));
            }
        }
        if let Some((decls, stmts)) = others {
            self.lower_gen_decls(module, &decls, false);
            for st in &stmts {
                self.lower_conc_stmt(module, st, proc_idx);
            }
        }
    }

    /// `block [is] decls begin stmts end block;` — decls and statements are
    /// lowered into the enclosing module (block scoping is not modeled).
    fn lower_block(&mut self, module: &mut ModuleIr, s: &SyntaxNode, proc_idx: &mut usize) {
        let kids = real_children(s);
        let (decls, stmts) = self.gen_body(&kids);
        self.lower_gen_decls(module, &decls, false);
        for st in &stmts {
            self.lower_conc_stmt(module, st, proc_idx);
        }
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
        let Some(target) = kids[is_pos + 1..].iter().find(|k| is_expr_node(k.kind())) else {
            return;
        };
        let e = self.lower_expr(target, &self.consts.clone());
        match &e {
            Expr::Sig(id) => {
                self.names.insert(name.clone(), *id);
            }
            Expr::Lit(v) => {
                self.consts.insert(name.clone(), v.clone());
            }
            _ => {}
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
        let init = kids[colon + 1..]
            .iter()
            .find(|k| is_expr_node(k.kind()) && k.kind() != SyntaxKind::Name);
        let span = d.span();
        let type_name = type_mark.map(|tm| self.text(tm).to_ascii_lowercase());
        let (width, signed, mem, real) = match type_mark {
            Some(tm) => self.resolve_type(tm, constraint, span),
            None => (1, false, None, false),
        };
        let kind = if real { SigKind::Real } else { SigKind::Wire };
        let init_v = init.and_then(|e| {
            let ex = self.lower_expr(e, &self.consts.clone());
            self.ce(&ex)
        });
        for n in &names {
            if self.sig_id(n).is_some() {
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
            if let Some(tn) = &type_name {
                match self.types.get(tn) {
                    Some(TypeInfo::Record { .. }) => {
                        self.sig_rec.insert(id, tn.clone());
                    }
                    // Array: remember the element record type for
                    // `arr(i).field` selects (arrays of records), and the
                    // dims from the signal's own constraint if present, else
                    // from the array type declaration.
                    Some(TypeInfo::Array {
                        dims, elem_name, ..
                    }) => {
                        if let Some(en) = elem_name {
                            if matches!(self.types.get(en), Some(TypeInfo::Record { .. })) {
                                self.sig_elem_rec.insert(id, en.clone());
                            }
                        }
                        if !dims.is_empty() {
                            let d = match constraint {
                                Some(c) => self.constraint_dims(c),
                                None => dims.clone(),
                            };
                            self.sig_dims.insert(id, d);
                        }
                    }
                    _ => {}
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
                None => String::new(),
            };
            self.consts.insert(n, string_value(&p));
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
        let Some(init) = init else { return };
        let env = self.consts.clone();
        let width = type_mark
            .map(|tm| self.resolve_type(tm, None, d.span()).0)
            .unwrap_or(32);
        // Record-typed constant: order named aggregate fields by the record.
        let ex = match type_mark {
            Some(tm)
                if init.kind() == SyntaxKind::Aggregate
                    && matches!(
                        self.types.get(&self.text(tm).to_ascii_lowercase()),
                        Some(TypeInfo::Record { .. })
                    ) =>
            {
                let tn = self.text(tm).to_ascii_lowercase();
                let fields = self.record_fields_of_type(&tn);
                self.lower_record_aggregate(&init, &fields, &env)
            }
            _ => self.lower_expr(&init, &env),
        };
        if let Some(tm) = &type_mark {
            let tn = self.text(tm).to_ascii_lowercase();
            for n in &names {
                self.const_types.insert(n.to_ascii_lowercase(), tn.clone());
            }
        }
        match self.ce_env(&ex, &env) {
            Some(v) => {
                for n in &names {
                    self.consts.insert(n.to_ascii_lowercase(), v.clone());
                }
            }
            None => {
                // Keep the name bound to X (e.g. a generic with no default is
                // not constant at analysis time) so uses resolve instead of
                // cascading into unknown-identifier errors.
                self.warn(
                    d.span(),
                    "constant initializer is not constant; treated as X",
                );
                for n in &names {
                    self.consts
                        .insert(n.to_ascii_lowercase(), Value::x(width.max(1)));
                }
            }
        }
    }

    fn lower_type_decl(&mut self, d: &SyntaxNode) {
        let kids = real_children(d);
        // kids[0] is the `type` keyword; the name is the next ident.
        let Some(name_node) = kids.iter().filter(|k| k.kind() == SyntaxKind::Ident).nth(1) else {
            return;
        };
        let name = self.kw(name_node);
        // Record: `type t is record f : typ [constraint]; ... end record;`
        // (checked before enum: field constraints contain `(` too).
        if kids
            .iter()
            .any(|k| k.kind() == SyntaxKind::Ident && self.kw(k) == "record")
        {
            // Fields: `name [, name...] : type [constraint];` — comma-separated
            // field names share one type mark.
            let mut fields: Vec<(String, usize, bool)> = Vec::new();
            let mut i = 0;
            while i < kids.len() {
                if kids[i].kind() == SyntaxKind::Name {
                    let mut j = i;
                    let mut names: Vec<String> = Vec::new();
                    while j < kids.len() && kids[j].kind() == SyntaxKind::Name {
                        names.push(self.text(&kids[j]).to_ascii_lowercase());
                        j += 1;
                        if kids.get(j).map_or(false, |p| {
                            p.kind() == SyntaxKind::Punct && self.text(p) == ","
                        }) {
                            j += 1;
                        } else {
                            break;
                        }
                    }
                    if kids.get(j).map_or(false, |p| {
                        p.kind() == SyntaxKind::Punct && self.text(p) == ":"
                    }) {
                        if let Some(tm) = kids.get(j + 1).filter(|k| k.kind() == SyntaxKind::Name) {
                            let constraint =
                                kids.get(j + 2).filter(|k| k.kind() == SyntaxKind::Range);
                            let (w, signed, _, _) = self.resolve_type(tm, constraint, d.span());
                            for n in names {
                                fields.push((n, w.max(1), signed));
                            }
                            i = j + 2;
                            continue;
                        }
                    }
                }
                i += 1;
            }
            if !fields.is_empty() {
                self.types.insert(name, TypeInfo::Record { fields });
            }
            return;
        }
        // Enum: `type t is (a, b, c);`
        let enum_lits: Vec<String> = kids
            .iter()
            .skip_while(|k| !(k.kind() == SyntaxKind::Punct && self.text(k) == "("))
            .filter(|k| k.kind() == SyntaxKind::Ident || k.kind() == SyntaxKind::CharLiteral)
            .map(|k| self.text(k).trim_matches('\'').to_ascii_lowercase())
            .collect();
        if !enum_lits.is_empty() {
            for (i, l) in enum_lits.iter().enumerate() {
                self.consts.insert(l.clone(), Value::from_u64(32, i as u64));
            }
            self.types.insert(name, TypeInfo::Enum(enum_lits));
            return;
        }
        // Array: `type t is array (range) of elem;`
        if kids
            .iter()
            .any(|k| k.kind() == SyntaxKind::Ident && self.kw(k) == "array")
        {
            let range = kids.iter().find(|k| k.kind() == SyntaxKind::Range);
            let elem_pos = kids.iter().rposition(|k| k.kind() == SyntaxKind::Name);
            let elem = elem_pos.map(|i| kids[i].clone());
            // Element constraint, if any: `... of std_logic_vector(31 downto 0)`.
            let elem_constraint = elem_pos.and_then(|i| {
                kids.iter()
                    .skip(i + 1)
                    .find(|k| k.kind() == SyntaxKind::Range)
            });
            let dims = range.map(|r| self.constraint_dims(r)).unwrap_or_default();
            let depth = if dims.is_empty() {
                range.map(|r| self.constraint_width(r)).unwrap_or(1).max(1)
            } else {
                dims.iter().map(|d| d.1).product::<usize>().max(1)
            };
            let elem_name = elem.as_ref().map(|e| self.text(e).to_ascii_lowercase());
            let (ew, _, _, _) = match elem {
                Some(e) => self.resolve_type(&e, elem_constraint, d.span()),
                None => (8, false, None, false),
            };
            self.types.insert(
                name,
                TypeInfo::Array {
                    elem: ew,
                    depth,
                    dims,
                    elem_name,
                },
            );
            return;
        }
        // Range type: `type t is range 0 to 10;` → integer
        if kids
            .iter()
            .any(|k| k.kind() == SyntaxKind::Ident && self.kw(k) == "range")
        {
            // The ieee fixed/float types (`type ufixed is range <>;`) map to
            // real; plain integer ranges stay integer.
            if matches!(name.as_str(), "ufixed" | "sfixed" | "float") {
                self.types.insert(name, TypeInfo::Real);
            } else {
                self.types.insert(name, TypeInfo::Integer { signed: false });
            }
        }
    }

    fn lower_subtype_decl(&mut self, d: &SyntaxNode) {
        let kids = real_children(d);
        // kids[0] is the `subtype` keyword; the name is the next ident.
        let name = kids
            .iter()
            .filter(|k| k.kind() == SyntaxKind::Ident)
            .nth(1)
            .map(|k| self.kw(k));
        let Some(name) = name else { return };
        // `subtype s is base [(constraint)] [range ...]`
        let is_pos = kids
            .iter()
            .position(|k| k.kind() == SyntaxKind::Ident && self.kw(k) == "is");
        let Some(is_pos) = is_pos else { return };
        let base = kids[is_pos + 1..]
            .iter()
            .find(|k| k.kind() == SyntaxKind::Name)
            .cloned();
        let constraint = kids[is_pos + 1..]
            .iter()
            .find(|k| k.kind() == SyntaxKind::Range)
            .cloned();
        if let Some(base) = base {
            let span = d.span();
            let base_name = self.kw(&base);
            let (w, signed, mem, _r) = self.resolve_type(&base, constraint.as_ref(), span);
            let _ = mem;
            // Keep record/array base types as-is; vectors remember the
            // constrained width for use sites without their own constraint.
            match self.types.get(&base_name).cloned() {
                Some(ti @ (TypeInfo::Record { .. } | TypeInfo::Array { .. })) => {
                    self.types.insert(name, ti);
                }
                _ => {
                    if constraint.is_some() {
                        self.type_widths.insert(name.clone(), w.max(1));
                    }
                    self.types.insert(name, TypeInfo::Vector { signed });
                }
            }
        }
    }

    // ---- processes and statements ---------------------------------------------------

    /// Lower an architecture-level function declaration to FuncIr. The body is
    /// lowered in slot space: slot 0 = function name (return value), then
    /// parameters, then local variables.
    fn lower_function_decl(&mut self, module: &mut ModuleIr, f: &SyntaxNode) {
        if let Some(fi) = self.lower_function_node(f) {
            module.functions.push(fi);
        }
    }

    /// True when a function's local variable declarations constrain their
    /// width with a parameter (`variable v : std_ulogic_vector(n-1 downto 0)`),
    /// which cannot be lowered without knowing the call argument.
    fn function_locals_use_params(&self, f: &SyntaxNode) -> bool {
        let kids = real_children(f);
        let mut params: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut seen_begin = false;
        for k in &kids {
            if k.kind() == SyntaxKind::Ident && self.kw(k) == "begin" {
                seen_begin = true;
            }
            if !seen_begin && k.kind() == SyntaxKind::PortDecl {
                let pk = real_children(k);
                if let Some(colon) = pk
                    .iter()
                    .position(|c| c.kind() == SyntaxKind::Punct && self.text(c) == ":")
                {
                    for c in &pk[..colon] {
                        if c.kind() == SyntaxKind::Name {
                            params.insert(self.text(c).to_ascii_lowercase());
                        }
                    }
                }
            }
        }
        if params.is_empty() {
            return false;
        }
        kids.iter()
            .filter(|k| k.kind() == SyntaxKind::VariableDecl)
            .any(|d| {
                d.descendants()
                    .filter(|k| k.kind() == SyntaxKind::Name)
                    .any(|k| params.contains(&self.text(&k).to_ascii_lowercase()))
            })
    }

    fn lower_function_node(&mut self, f: &SyntaxNode) -> Option<FuncIr> {
        let kids = real_children(f);
        let mut name = String::new();
        let mut ret_mark: Option<SyntaxNode> = None;
        let mut param_decls: Vec<SyntaxNode> = Vec::new();
        let mut local_decls: Vec<SyntaxNode> = Vec::new();
        let mut stmt_nodes: Vec<SyntaxNode> = Vec::new();
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
                SyntaxKind::VariableDecl if !seen_begin => local_decls.push(k.clone()),
                // Function-local constants stay in `consts` (the body lowers in
                // slot space); runtime indexing of them lowers to `$const_idx`.
                SyntaxKind::ConstantDecl if !seen_begin => self.lower_constant_decl(k),
                _ if seen_begin && is_stmt_node(k.kind()) => stmt_nodes.push(k.clone()),
                _ => {}
            }
        }
        if name.is_empty() {
            return None;
        }
        let (mut ret_width, _, _, _) = match &ret_mark {
            Some(tm) => self.resolve_type(tm, None, f.span()),
            None => (32, false, None, false),
        };
        // Params: PortDecl `a, b : integer [range]` — names before ':', type after.
        let mut args: Vec<(String, usize)> = Vec::new();
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
            let constraint = colon
                .and_then(|c| pk.get(c + 2))
                .filter(|k| k.kind() == SyntaxKind::Range);
            let (w, _, _, _) = match tmark {
                Some(tm) => self.resolve_type(tm, constraint, pd.span()),
                None => (32, false, None, false),
            };
            for n in names {
                args.push((n, w));
            }
        }
        // Local variables.
        let mut locals: Vec<SignalIr> = Vec::new();
        for d in &local_decls {
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
            for n in names {
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
        // Lower the body in slot space (slot 0 = return value).
        let saved_names = std::mem::take(&mut self.names);
        let mut slots: Vec<(String, usize)> = vec![(name.clone(), ret_width.max(1))];
        slots.extend(args.iter().cloned());
        slots.extend(locals.iter().map(|l| (l.name.clone(), l.width)));
        self.names = slots
            .iter()
            .enumerate()
            .map(|(i, (n, _))| (n.clone(), i as SigId))
            .collect();
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
        for sig in self.extra_signals.drain(..) {
            locals.push(sig);
        }
        self.extra_base = 0;
        self.names = saved_names;
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
        Some(FuncIr {
            name,
            ret_width: ret_width.max(1),
            args,
            locals,
            body,
        })
    }

    /// Estimated bit width of a lowered expression (lowerer-side; used to give
    /// unconstrained return types the width of what is actually returned).
    fn expr_width_est(&self, e: &Expr) -> usize {
        match e {
            Expr::Lit(v) => v.width(),
            Expr::Real(_) => 64,
            Expr::Sig(s) => self.sig_width.get(s).copied().unwrap_or(32).max(1),
            Expr::Bin { l, r, .. } => self.expr_width_est(l).max(self.expr_width_est(r)),
            Expr::Un { e, .. } => self.expr_width_est(e),
            Expr::Cond { t, e, .. } => self.expr_width_est(t).max(self.expr_width_est(e)),
            Expr::Concat(parts) => parts
                .iter()
                .map(|p| self.expr_width_est(p))
                .sum::<usize>()
                .max(1),
            Expr::Sel { msb, lsb, .. } => {
                let a = self.ce(msb).and_then(|v| v.to_u64()).unwrap_or(0) as usize;
                let b = self.ce(lsb).and_then(|v| v.to_u64()).unwrap_or(0) as usize;
                a.abs_diff(b) + 1
            }
            Expr::Idx { e, .. } => self.expr_width_est(e),
            Expr::Call { name, .. } => self
                .funcs
                .get(&name.to_ascii_lowercase())
                .map(|f| f.ret_width)
                .unwrap_or(32)
                .max(1),
            Expr::Replicate { count, e } => {
                self.ce(count).and_then(|v| v.to_u64()).unwrap_or(1) as usize
                    * self.expr_width_est(e)
            }
            Expr::Resize { width, .. } => *width,
            Expr::Signed(e) => self.expr_width_est(e),
            Expr::ToString(_) => 8 * 12,
            _ => 32,
        }
    }

    fn lower_process(&mut self, module: &mut ModuleIr, p: &SyntaxNode, idx: usize) -> ProcessIr {
        let kids = real_children(p);
        let mut sens = Sens::List(Vec::new());
        let mut stmt_nodes: Vec<SyntaxNode> = Vec::new();
        for k in &kids {
            match k.kind() {
                SyntaxKind::SensList => {
                    sens = self.lower_sens_list(k);
                }
                SyntaxKind::SignalDecl
                | SyntaxKind::VariableDecl
                | SyntaxKind::ConstantDecl
                | SyntaxKind::TypeDecl
                | SyntaxKind::SubtypeDecl => {
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
        let origin = format!("process #{}", idx);
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
        let init = kids[colon + 1..]
            .iter()
            .find(|k| is_expr_node(k.kind()) && k.kind() != SyntaxKind::Name);
        let span = d.span();
        let type_name = type_mark.map(|tm| self.text(tm).to_ascii_lowercase());
        let (width, signed, _, real) = match type_mark {
            Some(tm) => self.resolve_type(tm, constraint, span),
            None => (32, false, None, false),
        };
        let init_v = init.and_then(|e| {
            let ex = self.lower_expr(e, &self.consts.clone());
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
            if let Some(tn) = &type_name {
                match self.types.get(tn) {
                    Some(TypeInfo::Record { .. }) => {
                        self.sig_rec.insert(id, tn.clone());
                    }
                    Some(TypeInfo::Array {
                        dims, elem_name, ..
                    }) => {
                        if let Some(en) = elem_name {
                            if matches!(self.types.get(en), Some(TypeInfo::Record { .. })) {
                                self.sig_elem_rec.insert(id, en.clone());
                            }
                        }
                        if !dims.is_empty() {
                            let dd = match constraint {
                                Some(c) => self.constraint_dims(c),
                                None => dims.clone(),
                            };
                            self.sig_dims.insert(id, dd);
                        }
                    }
                    _ => {}
                }
            }
            self.names.insert(n.clone(), id);
        }
    }

    fn lower_sens_list(&mut self, s: &SyntaxNode) -> Sens {
        let mut edges = Vec::new();
        for c in real_children(s) {
            if c.kind() == SyntaxKind::Name {
                let name = self.text(&c);
                // VHDL-2008 `process(all)`: sensitive to every signal read.
                if name == "all" {
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
                } else {
                    self.err(
                        c.span(),
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

    fn lower_conc_assign(&mut self, s: &SyntaxNode, idx: usize) -> Option<ConcOut> {
        let kids = real_children(s);
        let lhs_pos = kids.iter().position(|k| {
            k.kind() == SyntaxKind::Name
                || k.kind() == SyntaxKind::Call
                || k.kind() == SyntaxKind::PartSelect
        })?;
        let lhs = self.lower_expr(&kids[lhs_pos], &self.consts.clone());
        // Waveform elements: `expr [after t] [when c]`, comma-separated, or the
        // conditional form `expr when c else expr [when c]... [else expr]`.
        struct Elem {
            val: SyntaxNode,
            delay: Option<SyntaxNode>,
            cond: Option<SyntaxNode>,
        }
        let mut elems: Vec<Elem> = Vec::new();
        let mut cur: Option<Elem> = None;
        let mut i = lhs_pos + 1;
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
                        cur = Some(Elem {
                            val: e.clone(),
                            delay: None,
                            cond: None,
                        });
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                _ if is_expr_node(k.kind()) => {
                    if let Some(c) = cur.take() {
                        elems.push(c);
                    }
                    cur = Some(Elem {
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
        if elems.is_empty() {
            return None;
        }
        let has_cond = elems.iter().any(|e| e.cond.is_some());
        let has_delay = elems.iter().any(|e| e.delay.is_some());
        if has_cond && has_delay {
            self.err(s.span(), "conditional waveform with `after` is unsupported");
            return None;
        }
        if has_delay {
            // A delayed concurrent assignment is a one-shot process scheduling
            // each transaction; VHDL waveform delays are each relative to the
            // time the statement executes (not cumulative like Verilog).
            let mut stmts = Vec::new();
            let mut prev = 0u64;
            for e in &elems {
                let mut t = 0u64;
                if let Some(d) = &e.delay {
                    let dv = self.lower_expr(d, &self.consts.clone());
                    match self.ce(&dv).and_then(|v| v.to_u64()) {
                        Some(dt) => t = dt,
                        None => {
                            self.err(d.span(), "waveform delay must be constant");
                            return None;
                        }
                    }
                }
                let val = self.lower_rhs(&kids[lhs_pos], &e.val, &self.consts.clone());
                let a = Stmt::Assign {
                    lhs: lhs.clone(),
                    rhs: val,
                    nba: false,
                };
                stmts.push(if t > prev {
                    Stmt::Delay {
                        time_fs: t - prev,
                        stmt: Box::new(a),
                    }
                } else {
                    a
                });
                prev = t;
            }
            let body = if stmts.len() == 1 {
                stmts.pop().unwrap()
            } else {
                Stmt::Block(stmts)
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
                let v = self.lower_rhs(&kids[lhs_pos], &e.val, &self.consts.clone());
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
        // Plain waveform: the last element at t=0 wins.
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
        // variant module (see lower_architecture).
        let unit = unit_text.rsplit('.').next().unwrap_or("").to_string();
        let name = match arch {
            Some(a) => format!("{unit}/{a}"),
            None => unit,
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
                    let name_pos = ck.iter().position(|c| c.kind() == SyntaxKind::Name);
                    let assoc_name = name_pos.map(|i| self.text(&ck[i]));
                    let expr = ck
                        .iter()
                        .enumerate()
                        .rev()
                        .find(|(i, c)| Some(*i) != name_pos && is_expr_node(c.kind()))
                        .map(|(_, c)| c);
                    let Some(e) = expr else { continue };
                    let ex = self.lower_expr(e, &self.consts.clone());
                    match assoc_name {
                        Some(n) if in_generic => param_overrides.push((n, ex)),
                        Some(n) => conns.push(Conn::Named(n, ex)),
                        None => conns.push(Conn::Positional(ex)),
                    }
                }
                _ => {}
            }
        }
        Some(InstIr {
            module: name,
            name: String::new(), // label handled by elaboration naming
            param_overrides,
            conns,
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
            stmts.into_iter().next().unwrap()
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

    fn lower_seq_stmt_impl(&mut self, s: &SyntaxNode) -> Stmt {
        match s.kind() {
            SyntaxKind::SeqAssign => {
                let kids = real_children(s);
                // `(a, b) := f(...);` — parenthesized target list: not
                // lowered (no error).
                if kids
                    .first()
                    .map_or(false, |k| k.kind() == SyntaxKind::Punct)
                {
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
                    (Some(l), Some(r)) => Stmt::Assign {
                        lhs: self.lower_expr(l, &self.consts.clone()),
                        rhs: self.lower_rhs(l, r, &self.consts.clone()),
                        nba: true, // VHDL signal assignment is delta-delayed
                    },
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
                    (Some(l), Some(r)) => Stmt::Assign {
                        lhs: self.lower_expr(l, &self.consts.clone()),
                        rhs: self.lower_rhs(l, r, &self.consts.clone()),
                        nba: false,
                    },
                    _ => Stmt::Null,
                }
            }
            SyntaxKind::If => self.lower_if(s),
            SyntaxKind::Case => self.lower_case(s),
            SyntaxKind::For => self.lower_for(s),
            SyntaxKind::LoopStmt => self.lower_loop(s),
            SyntaxKind::Wait => self.lower_wait(s),
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
                // `exit;` / `exit when cond;` → break (optionally conditional).
                let kids = real_children(s);
                let cond = kids
                    .iter()
                    .find(|k| is_expr_node(k.kind()))
                    .map(|k| self.lower_expr(k, &self.consts.clone()));
                match cond {
                    Some(c) => Stmt::If {
                        cond: c,
                        then: Box::new(Stmt::Break),
                        els: None,
                    },
                    None => Stmt::Break,
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
                // textio arguments are frequently plain names (`output`, `l`);
                // for other calls keep skipping bare names (named associations).
                let args: Vec<Expr> = src
                    .iter()
                    .enumerate()
                    .filter(|(i, k)| Some(*i) != name_pos && is_expr_node(k.kind()))
                    .filter(|(_, k)| is_textio || k.kind() != SyntaxKind::Name)
                    .map(|(_, k)| self.lower_expr(k, &self.consts.clone()))
                    .collect();
                let name = if is_textio {
                    format!("$textio_{fname}")
                } else {
                    fname
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
                    ss.into_iter().next().unwrap()
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
                let rk = real_children(range.unwrap());
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
                let ak = real_children(range.unwrap());
                let base = ak.iter().find(|k| k.kind() == SyntaxKind::Name);
                if let Some(b) = base {
                    let bn = self.text(b);
                    if let Some(id) = self.sig_id(&bn) {
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
                        // a separate Ident sibling.
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
                            time_fs = Some(v * mult);
                            i += 1;
                        }
                    }
                    _ => {}
                }
            }
            i += 1;
        }
        // `wait for 10 ns` → timed delay (takes precedence over a condition).
        if let Some(t) = time_fs {
            return Stmt::Delay {
                time_fs: t,
                stmt: Box::new(Stmt::Null),
            };
        }
        // `wait until rising_edge(clk)` → edge wait; `wait until expr` → any-edge on expr's signals.
        if let Some(c) = &cond {
            if let Some(e) = edges_from_cond(c, self) {
                return Stmt::Wait {
                    sens: e,
                    body: Box::new(Stmt::Null),
                };
            }
        }
        if let Some(sl) = sens.as_ref() {
            if sl.kind() == SyntaxKind::SensList {
                return Stmt::Wait {
                    sens: match self.lower_sens_list(sl) {
                        Sens::List(es) => es,
                        Sens::All => Vec::new(),
                    },
                    body: Box::new(Stmt::Null),
                };
            }
        }
        // Bare `wait;` — park forever.
        Stmt::Wait {
            sens: Vec::new(),
            body: Box::new(Stmt::Null),
        }
    }

    // ---- expressions ---------------------------------------------------------------

    fn lower_expr(&mut self, e: &SyntaxNode, env: &HashMap<String, Value>) -> Expr {
        match e.kind() {
            SyntaxKind::Literal => self.lower_literal(e, env),
            SyntaxKind::ParenExpr => {
                let inner = real_children(e)
                    .into_iter()
                    .find(|k| is_expr_node(k.kind()));
                match inner {
                    Some(k) => self.lower_expr(&k, env),
                    None => Expr::Lit(Value::x(1)),
                }
            }
            SyntaxKind::Name => {
                let name = self.text(e);
                // `arr(i).field` — the parser folds this into one Name node
                // wrapping a Call plus `.field`. Lower the call, then select
                // the record field inside the element (slice of the memory
                // element for arrays of records).
                let ekids = real_children(e);
                if let Some(call) = ekids.iter().find(|k| k.kind() == SyntaxKind::Call).cloned() {
                    let field = ekids
                        .iter()
                        .filter(|k| k.kind() == SyntaxKind::Ident)
                        .last()
                        .map(|k| self.kw(k))
                        .unwrap_or_default();
                    let base = self.lower_expr(&call, env);
                    let rec_name = match &base {
                        Expr::Idx { e: be, .. } => match &**be {
                            Expr::Sig(id) => self
                                .sig_elem_rec
                                .get(id)
                                .or_else(|| self.sig_rec.get(id))
                                .cloned(),
                            _ => None,
                        },
                        Expr::Sig(id) => self
                            .sig_rec
                            .get(id)
                            .or_else(|| self.sig_elem_rec.get(id))
                            .cloned(),
                        // `const_arr(i).field` lowered via `$const_idx`: the
                        // element record type comes from the constant's type.
                        Expr::Call { name, .. } if name == "$const_idx" => {
                            let ck = real_children(&call);
                            let cn = ck
                                .iter()
                                .find(|k| k.kind() == SyntaxKind::Name)
                                .map(|k| self.text(k))
                                .unwrap_or_default();
                            let cn = cn
                                .rsplit('.')
                                .next()
                                .unwrap_or(cn.as_str())
                                .to_ascii_lowercase();
                            self.const_types
                                .get(&cn)
                                .and_then(|tn| match self.types.get(tn) {
                                    Some(TypeInfo::Array {
                                        elem_name: Some(en),
                                        ..
                                    }) => Some(en.clone()),
                                    _ => None,
                                })
                        }
                        _ => None,
                    };
                    if let Some(tn) = rec_name {
                        if let Some(TypeInfo::Record { fields }) = self.types.get(&tn) {
                            let total: usize = fields.iter().map(|f| f.1).sum();
                            let mut run = 0usize;
                            for (fn_, w, _) in fields {
                                let lsb = total - run - w;
                                if *fn_ == field {
                                    return Expr::Sel {
                                        e: Box::new(base),
                                        msb: Box::new(Expr::Lit(Value::from_u64(
                                            32,
                                            (lsb + w - 1) as u64,
                                        ))),
                                        lsb: Box::new(Expr::Lit(Value::from_u64(32, lsb as u64))),
                                    };
                                }
                                run += w;
                            }
                        }
                    }
                }
                // Record field select: `r.field` on a record-typed signal →
                // slice of the flat record vector (first field = MSB).
                if let Some(dot) = name.find('.') {
                    let base = name[..dot].to_ascii_lowercase();
                    let field = name[dot + 1..].to_ascii_lowercase();
                    let id = self
                        .sig_id(&base)
                        .or_else(|| self.names.get(&format!("__const_{}", base)).copied());
                    if let Some(id) = id {
                        if let Some(tn) = self.sig_rec.get(&id).cloned() {
                            if let Some(TypeInfo::Record { fields }) = self.types.get(&tn) {
                                let total: usize = fields.iter().map(|f| f.1).sum();
                                let mut run = 0usize;
                                for (fn_, w, _) in fields {
                                    let lsb = total - run - w;
                                    if *fn_ == field {
                                        return Expr::Sel {
                                            e: Box::new(Expr::Sig(id)),
                                            msb: Box::new(Expr::Lit(Value::from_u64(
                                                32,
                                                (lsb + w - 1) as u64,
                                            ))),
                                            lsb: Box::new(Expr::Lit(Value::from_u64(
                                                32, lsb as u64,
                                            ))),
                                        };
                                    }
                                    run += w;
                                }
                            }
                        }
                    } else if let (Some(v), Some(tn)) = (
                        self.consts.get(&base).cloned(),
                        self.const_types.get(&base).cloned(),
                    ) {
                        // `rec_const.field` on a record-typed constant: slice the
                        // literal value directly.
                        if let Some(TypeInfo::Record { fields }) = self.types.get(&tn) {
                            let total: usize = fields.iter().map(|f| f.1).sum();
                            let mut run = 0usize;
                            for (fn_, w, _) in fields {
                                let lsb = total - run - w;
                                if *fn_ == field {
                                    return Expr::Sel {
                                        e: Box::new(Expr::Lit(v)),
                                        msb: Box::new(Expr::Lit(Value::from_u64(
                                            32,
                                            (lsb + w - 1) as u64,
                                        ))),
                                        lsb: Box::new(Expr::Lit(Value::from_u64(32, lsb as u64))),
                                    };
                                }
                                run += w;
                            }
                        }
                    }
                }
                let base = name.rsplit('.').next().unwrap_or(name.as_str());
                let key = base.to_ascii_lowercase();
                // Local scope (env: loop vars, params) first, then signals, then
                // constants — so an enum literal from a `use`d package (e.g. 'U'
                // from std_logic_1164) cannot shadow a signal named `u`.
                if let Some(v) = env.get(&key) {
                    return Expr::Lit(v.clone());
                }
                if let Some(id) = self.sig_id(base) {
                    return Expr::Sig(id);
                }
                if let Some(v) = self.consts.get(&key) {
                    return Expr::Lit(v.clone());
                }
                if let Some(a) = self.aliases.get(&key) {
                    return a.clone();
                }
                // Parameterless function called without parentheses: `c_hi`.
                if let Some(f) = self.funcs.get(&key) {
                    if f.args.is_empty() {
                        return Expr::Call {
                            name: f.name.clone(),
                            args: Vec::new(),
                        };
                    }
                }
                // `open` (reserved word) in a port/generic map association:
                // an unconnected port — X, no diagnostic.
                if key == "open" {
                    return Expr::Lit(Value::x(1));
                }
                self.err(e.span(), format!("unknown identifier '{}'", base));
                Expr::Lit(Value::x(1))
            }
            SyntaxKind::BinExpr => {
                let kids = real_children(e);
                let exprs: Vec<&SyntaxNode> =
                    kids.iter().filter(|k| is_expr_node(k.kind())).collect();
                let op_text: String = kids
                    .iter()
                    .filter(|k| k.kind() == SyntaxKind::Punct || k.kind() == SyntaxKind::Ident)
                    .map(|k| self.kw(k))
                    .collect::<Vec<_>>()
                    .join("");
                if op_text == "&" && exprs.len() == 2 {
                    // Concatenation.
                    return Expr::Concat(vec![
                        self.lower_expr(exprs[0], env),
                        self.lower_expr(exprs[1], env),
                    ]);
                }
                let Some(op) = vhdl_binop(&op_text) else {
                    self.err(e.span(), format!("unsupported operator '{}'", op_text));
                    return Expr::Lit(Value::x(1));
                };
                if exprs.len() == 2 {
                    Expr::Bin {
                        op,
                        l: Box::new(self.lower_expr(exprs[0], env)),
                        r: Box::new(self.lower_expr(exprs[1], env)),
                    }
                } else {
                    Expr::Lit(Value::x(1))
                }
            }
            SyntaxKind::UnaryExpr => {
                let kids = real_children(e);
                let op_text = kids
                    .iter()
                    .find(|k| k.kind() == SyntaxKind::Punct || k.kind() == SyntaxKind::Ident)
                    .map(|k| self.kw(k))
                    .unwrap_or_default();
                let operand = kids.iter().find(|k| is_expr_node(k.kind()));
                let Some(op) = vhdl_unop(&op_text) else {
                    self.err(
                        e.span(),
                        format!("unsupported unary operator '{}'", op_text),
                    );
                    return Expr::Lit(Value::x(1));
                };
                match operand {
                    Some(o) => Expr::Un {
                        op,
                        e: Box::new(self.lower_expr(o, env)),
                    },
                    None => Expr::Lit(Value::x(1)),
                }
            }
            SyntaxKind::Call => self.lower_call(e, env),
            SyntaxKind::PartSelect => {
                // name ( msb dir lsb ) — the base name is not a bound expr.
                let kids = real_children(e);
                let base_pos = kids.iter().position(|k| k.kind() == SyntaxKind::Name);
                let base = base_pos.map(|i| &kids[i]);
                let exprs: Vec<&SyntaxNode> = kids
                    .iter()
                    .enumerate()
                    .filter(|(i, k)| Some(*i) != base_pos && is_expr_node(k.kind()))
                    .map(|(_, k)| k)
                    .collect();
                if exprs.len() < 2 {
                    return Expr::Lit(Value::x(1));
                }
                let dir = kids
                    .iter()
                    .find(|k| k.kind() == SyntaxKind::Ident)
                    .map(|k| self.kw(k))
                    .unwrap_or_else(|| "downto".to_string());
                let (msb, lsb) = if dir == "downto" {
                    (exprs[0], exprs[1])
                } else {
                    (exprs[1], exprs[0])
                };
                let base_e = match base {
                    Some(b) => self.lower_expr(b, env),
                    None => Expr::Lit(Value::x(1)),
                };
                Expr::Sel {
                    e: Box::new(base_e),
                    msb: Box::new(self.lower_expr(msb, env)),
                    lsb: Box::new(self.lower_expr(lsb, env)),
                }
            }
            SyntaxKind::Attr => {
                // name'attr — only 'length/'high/'low/'range supported statically.
                let kids = real_children(e);
                let base = kids.iter().find(|k| k.kind() == SyntaxKind::Name);
                let attr = kids
                    .iter()
                    .filter(|k| k.kind() == SyntaxKind::Ident)
                    .last()
                    .map(|k| self.kw(k));
                let Some(attr) = attr else {
                    return Expr::Lit(Value::x(1));
                };
                let Some(b) = base else {
                    return Expr::Lit(Value::x(1));
                };
                let bn = self.text(b);
                match attr.as_str() {
                    "length" | "high" | "low" | "left" | "right" => {
                        // Statically known for signals: arrays → element count
                        // (mem depth), vectors → bit width.
                        if let Some(id) = self.sig_id(&bn) {
                            if self.uncon_ports.contains(&id) {
                                // Width known only after port connection.
                                return Expr::SigAttr {
                                    sig: id,
                                    attr: match attr.as_str() {
                                        "length" => "length",
                                        "high" => "high",
                                        "low" => "low",
                                        "left" => "left",
                                        _ => "right",
                                    },
                                };
                            }
                            // Arrays: element count = product of dimension sizes
                            // (sig_dims when present, else mem depth); vectors: bit
                            // width. `low` is the first dimension's left.
                            let dims = self.sig_dims.get(&id);
                            let n: usize = dims
                                .and_then(|d| {
                                    if d.is_empty() {
                                        None
                                    } else {
                                        Some(d.iter().map(|(_, s)| (*s).max(1)).product::<usize>())
                                    }
                                })
                                .or_else(|| self.mems.get(&id).copied())
                                .or_else(|| self.sig_width.get(&id).copied())
                                .unwrap_or(1);
                            let lo: i64 =
                                dims.and_then(|d| d.first().map(|(l, _)| *l)).unwrap_or(0);
                            let v = match attr.as_str() {
                                "length" => n as u64,
                                "high" | "left" => (lo + n as i64 - 1).max(0) as u64,
                                _ => lo.max(0) as u64, // low | right
                            };
                            return Expr::Lit(Value::from_u64(32, v));
                        }
                        Expr::Lit(Value::x(32))
                    }
                    // `sig'last_value` — value before the current delta.
                    "last_value" => {
                        if let Some(id) = self.sig_id(&bn) {
                            Expr::Prev(Box::new(Expr::Sig(id)))
                        } else {
                            self.warn(
                                e.span(),
                                format!("attribute '{}\\'{}' not supported; X", bn, attr),
                            );
                            Expr::Lit(Value::x(1))
                        }
                    }
                    // `sig'stable` — no event this delta: NOT (rising OR falling).
                    "stable" => {
                        if let Some(id) = self.sig_id(&bn) {
                            let s = || Box::new(Expr::Sig(id));
                            Expr::Un {
                                op: UnOp::Not,
                                e: Box::new(Expr::Bin {
                                    op: BinOp::LogOr,
                                    l: Box::new(Expr::Rising(s())),
                                    r: Box::new(Expr::Falling(s())),
                                }),
                            }
                        } else {
                            self.warn(
                                e.span(),
                                format!("attribute '{}\\'{}' not supported; X", bn, attr),
                            );
                            Expr::Lit(Value::x(1))
                        }
                    }
                    // `sig'event` — true on any value change of sig this delta:
                    // rising OR falling edge (scalar signals).
                    "event" => {
                        if let Some(id) = self.sig_id(&bn) {
                            let s = || Box::new(Expr::Sig(id));
                            Expr::Bin {
                                op: BinOp::LogOr,
                                l: Box::new(Expr::Rising(s())),
                                r: Box::new(Expr::Falling(s())),
                            }
                        } else {
                            self.warn(
                                e.span(),
                                format!("attribute '{}\\'{}' not supported; X", bn, attr),
                            );
                            Expr::Lit(Value::x(1))
                        }
                    }
                    _ => {
                        self.warn(
                            e.span(),
                            format!("attribute '{}\'{}' not supported; X", bn, attr),
                        );
                        Expr::Lit(Value::x(1))
                    }
                }
            }
            SyntaxKind::Aggregate => self.lower_aggregate(e, env, 32),
            _ => {
                self.err(e.span(), format!("unsupported expression {:?}", e.kind()));
                Expr::Lit(Value::x(1))
            }
        }
    }

    fn lower_call(&mut self, e: &SyntaxNode, env: &HashMap<String, Value>) -> Expr {
        let kids = real_children(e);
        // `integer'image(x)` arrives as Call(Attr(Name integer, 'image), args).
        // The argument may itself be an attribute (`sig'last_value`, …), so
        // only the callee Attr is excluded — and only when it appears before
        // the callee Name (`a(a'high)` has the Attr as an *argument*).
        let name_pos0 = kids.iter().position(|k| k.kind() == SyntaxKind::Name);
        let attr_pos = kids
            .iter()
            .position(|k| k.kind() == SyntaxKind::Attr)
            .filter(|&i| name_pos0.map_or(true, |np| i < np));
        if let Some(ap) = attr_pos {
            let ak = real_children(&kids[ap]);
            let attr_name = ak
                .iter()
                .filter(|k| k.kind() == SyntaxKind::Ident)
                .last()
                .map(|k| self.kw(k))
                .unwrap_or_default();
            if attr_name == "image" {
                if let Some(arg) = kids
                    .iter()
                    .enumerate()
                    .find(|(i, k)| *i != ap && is_expr_node(k.kind()))
                    .map(|(_, k)| k)
                {
                    return Expr::ToString(Box::new(self.lower_expr(arg, env)));
                }
            }
        }
        // The callee is the first Name child (absent for attribute calls like
        // `integer'image(x)`, where kids are [Attr, args...]). Arguments are
        // every other expression child — plain names included (`rising_edge(clk)`).
        let name_pos = kids.iter().position(|k| k.kind() == SyntaxKind::Name);
        let name = name_pos.map(|i| self.text(&kids[i])).unwrap_or_default();
        let fname = name
            .rsplit('.')
            .next()
            .unwrap_or(name.as_str())
            .to_ascii_lowercase();
        let args: Vec<Expr> = kids
            .iter()
            .enumerate()
            .filter(|(i, k)| Some(*i) != name_pos && Some(*i) != attr_pos && is_expr_node(k.kind()))
            .map(|(_, k)| self.lower_expr(k, env))
            .collect();
        // A record/array-typed constant is materialized as a hidden signal
        // (`__const_<name>`); index it like a signal.
        let base_id = self
            .sig_id(&name)
            .or_else(|| self.names.get(&format!("__const_{}", fname)).copied());
        if base_id.is_none() && !args.is_empty() {
            if let Some(e) = self.lower_const_array_index(&fname, &args) {
                return e;
            }
        }
        match fname.as_str() {
            // Indexed write/read of a signal: v(i) → Idx; multi-dimensional
            // v(i, j, …) → flat index ((i-lo0)*n1 + (j-lo1))*… via sig_dims.
            _ if base_id.is_some() && !args.is_empty() => {
                let base = base_id.unwrap();
                if let Some(dims) = self.sig_dims.get(&base).cloned() {
                    if dims.len() == args.len() {
                        let mut flat: Option<Expr> = None;
                        for (arg, (lo, n)) in args.iter().zip(dims.iter()) {
                            let a = Expr::Bin {
                                op: BinOp::Sub,
                                l: Box::new(arg.clone()),
                                r: Box::new(Expr::Lit(Value::from_u64(32, *lo as u64))),
                            };
                            flat = Some(match flat {
                                None => a,
                                Some(f) => Expr::Bin {
                                    op: BinOp::Add,
                                    l: Box::new(Expr::Bin {
                                        op: BinOp::Mul,
                                        l: Box::new(f),
                                        r: Box::new(Expr::Lit(Value::from_u64(32, *n as u64))),
                                    }),
                                    r: Box::new(a),
                                },
                            });
                        }
                        return Expr::Idx {
                            e: Box::new(Expr::Sig(base)),
                            i: Box::new(flat.unwrap()),
                        };
                    }
                }
                if args.len() == 1 {
                    return Expr::Idx {
                        e: Box::new(Expr::Sig(base)),
                        i: Box::new(args.into_iter().next().unwrap()),
                    };
                }
                Expr::Call { name: fname, args }
            }
            // Shift/rotate function forms from std_logic_arith & co.
            "shift_left" | "sll" | "sla" if args.len() == 2 => Expr::Bin {
                op: BinOp::Shl,
                l: Box::new(args[0].clone()),
                r: Box::new(args[1].clone()),
            },
            "shift_right" | "srl" if args.len() == 2 => Expr::Bin {
                op: BinOp::Shr,
                l: Box::new(args[0].clone()),
                r: Box::new(args[1].clone()),
            },
            "sra" if args.len() == 2 => Expr::Bin {
                op: BinOp::Sar,
                l: Box::new(args[0].clone()),
                r: Box::new(args[1].clone()),
            },
            "rotate_left" | "rol" if args.len() == 2 => Expr::Bin {
                op: BinOp::Rol,
                l: Box::new(args[0].clone()),
                r: Box::new(args[1].clone()),
            },
            "rotate_right" | "ror" if args.len() == 2 => Expr::Bin {
                op: BinOp::Ror,
                l: Box::new(args[0].clone()),
                r: Box::new(args[1].clone()),
            },
            "rising_edge" if args.len() == 1 => {
                Expr::Rising(Box::new(args.into_iter().next().unwrap()))
            }
            "falling_edge" if args.len() == 1 => {
                Expr::Falling(Box::new(args.into_iter().next().unwrap()))
            }
            "to_integer" | "conv_integer" | "integer" | "natural" | "positive"
                if args.len() == 1 =>
            {
                Expr::ToInt(Box::new(args.into_iter().next().unwrap()))
            }
            // Real/fixed/float conversions: `x * 1.0` (real-typed f64).
            "to_real" | "to_ufixed" | "to_sfixed" | "to_float" if !args.is_empty() => {
                as_real(args.into_iter().next().unwrap())
            }
            "resize" if args.len() == 3 => as_real(args.into_iter().next().unwrap()),
            "to_unsigned" | "conv_unsigned" | "conv_std_logic_vector" if args.len() == 2 => {
                let w = self
                    .ce_env(&args[1], env)
                    .and_then(|w| w.to_u64())
                    .unwrap_or(32) as usize;
                Expr::Resize {
                    e: Box::new(args.into_iter().next().unwrap()),
                    width: w,
                    signed: false,
                }
            }
            "to_signed" | "conv_signed" if args.len() == 2 => {
                let w = self
                    .ce_env(&args[1], env)
                    .and_then(|w| w.to_u64())
                    .unwrap_or(32) as usize;
                Expr::Resize {
                    e: Box::new(args.into_iter().next().unwrap()),
                    width: w,
                    signed: true,
                }
            }
            "resize" if args.len() == 2 => {
                let w = self
                    .ce_env(&args[1], env)
                    .and_then(|w| w.to_u64())
                    .unwrap_or(32) as usize;
                let inner = &args[0];
                let signed = match inner {
                    Expr::Sig(s) => self.sig_signed.get(s).copied().unwrap_or(false),
                    Expr::Signed(_) => true,
                    Expr::Resize { signed, .. } => *signed,
                    _ => false,
                };
                Expr::Resize {
                    e: Box::new(args.into_iter().next().unwrap()),
                    width: w,
                    signed,
                }
            }
            // `signed(x)` keeps the bits but marks signed semantics.
            "signed" if args.len() == 1 => Expr::Signed(Box::new(args.into_iter().next().unwrap())),
            // Other type conversions are bit-identical.
            "std_logic_vector" | "std_ulogic_vector" | "unsigned" | "bit_vector" | "std_logic"
            | "bit" | "boolean" | "string" | "char" | "real"
                if args.len() == 1 =>
            {
                args.into_iter().next().unwrap()
            }
            _ => Expr::Call { name: fname, args },
        }
    }

    fn lower_aggregate(
        &mut self,
        e: &SyntaxNode,
        env: &HashMap<String, Value>,
        width: usize,
    ) -> Expr {
        // `(others => x)` → replicate x; `(a, b, c)` → concat.
        let kids = real_children(e);
        if kids.iter().any(|k| k.kind() == SyntaxKind::Others) {
            let val = kids.iter().rev().find(|k| is_expr_node(k.kind()));
            return match val {
                Some(v) => {
                    let inner = self.lower_expr(v, env);
                    let iw = 1; // element width unknown statically; replicate by bits
                    let _ = iw;
                    Expr::Replicate {
                        count: Box::new(Expr::Lit(Value::from_u64(32, width as u64))),
                        e: Box::new(inner),
                    }
                }
                None => Expr::Lit(Value::x(width.max(1))),
            };
        }
        // Positional values only: a choice followed by `=>` is an association
        // name, not a value (record-typed targets use lower_record_aggregate).
        let mut parts: Vec<Expr> = Vec::new();
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
            if is_expr_node(k.kind()) && !arrow {
                parts.push(self.lower_expr(k, env));
            }
            i += 1;
        }
        Expr::Concat(parts)
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
        }
        self.lower_expr(rhs, env)
    }

    fn lower_literal(&mut self, e: &SyntaxNode, env: &HashMap<String, Value>) -> Expr {
        let t = self.text(e);
        // Physical literal: `100 ps` — the parser merges number + unit into
        // one Literal; the value is femtoseconds.
        if let Some((num, unit)) = t.split_once(char::is_whitespace) {
            if matches!(
                unit.trim().to_ascii_lowercase().as_str(),
                "fs" | "ps" | "ns" | "us" | "ms" | "sec" | "s" | "min" | "hr"
            ) {
                // Integer physical (`100 ps`) or real physical (`2.5 ns`,
                // VHDL-2008) \u2014 the value is femtoseconds.
                let fs = match self.parse_int_literal(num.trim()).and_then(|v| v.to_u64()) {
                    Some(n) => n * time_unit_fs(unit.trim()),
                    None => num
                        .trim()
                        .parse::<f64>()
                        .map(|f| (f * time_unit_fs(unit.trim()) as f64) as u64)
                        .unwrap_or(0),
                };
                return Expr::Lit(Value::from_u64(32, fs));
            }
        }
        // Bit string literal: x"FF" / b"01" / o"7".
        if t.len() >= 2
            && matches!(t.as_bytes()[0], b'x' | b'X' | b'b' | b'B' | b'o' | b'O')
            && t.as_bytes()[1] == b'"'
        {
            let base = match t.as_bytes()[0].to_ascii_lowercase() {
                b'x' => 16,
                b'o' => 8,
                _ => 2,
            };
            let digits = t[2..t.len() - 1].replace('_', "");
            let digit_bits = match base {
                16 => 4,
                8 => 3,
                _ => 1,
            };
            let width = digits.len() * digit_bits;
            let mut bits = vec![VX; width.max(1)];
            for (i, c) in digits.chars().enumerate() {
                let from_lsb = (digits.len() - 1 - i) * digit_bits;
                match c.to_ascii_lowercase() {
                    'x' | '?' => {}
                    'z' => {
                        for j in 0..digit_bits {
                            let idx = from_lsb + j;
                            if idx < bits.len() {
                                bits[idx] = VZ;
                            }
                        }
                    }
                    _ => {
                        if let Some(d) = c.to_digit(base) {
                            for j in 0..digit_bits {
                                let idx = from_lsb + j;
                                if idx < bits.len() {
                                    bits[idx] = if (d >> j) & 1 == 1 { V1 } else { V0 };
                                }
                            }
                        }
                    }
                }
            }
            return Expr::Lit(Value { bits });
        }
        if t.starts_with('\'') && t.ends_with('\'') && t.len() == 3 {
            // Character literal: std_logic value.
            let c = t.as_bytes()[1].to_ascii_lowercase();
            let b = match c {
                b'0' => V0,
                b'1' => V1,
                b'z' => VZ,
                _ => VX,
            };
            return Expr::Lit(Value { bits: vec![b] });
        }
        if t.starts_with('"') {
            let inner = t.trim_matches('"').replace("\"\"", "\"");
            // In VHDL a string of bit characters is a std_logic_vector value
            // (`"0000"`), not ASCII text — otherwise a plain string.
            if !inner.is_empty()
                && inner
                    .bytes()
                    .all(|c| matches!(c, b'0' | b'1' | b'x' | b'X' | b'z' | b'Z' | b'-' | b'?'))
            {
                let mut bits = vec![VX; inner.len()];
                for (i, c) in inner.bytes().enumerate() {
                    bits[inner.len() - 1 - i] = match c.to_ascii_lowercase() {
                        b'0' => V0,
                        b'1' => V1,
                        b'z' => VZ,
                        b'-' | b'?' => VDASH, // don't-care, matched by `case?`
                        _ => VX,
                    };
                }
                return Expr::Lit(Value { bits });
            }
            // String literal: first char in the high byte (MSB-first), so
            // `&`-concatenation and string_of read left to right.
            let bytes = inner.as_bytes();
            let n = bytes.len().max(1);
            let mut bits = vec![V0; n * 8];
            for (bi, &b) in bytes.iter().enumerate() {
                for j in 0..8 {
                    bits[(n - 1 - bi) * 8 + j] = if (b >> j) & 1 == 1 { V1 } else { V0 };
                }
            }
            return Expr::Lit(Value { bits });
        }
        if let Some(v) = self.parse_int_literal(&t) {
            return Expr::Lit(v);
        }
        if let Ok(v) = t.parse::<f64>() {
            // VHDL real literal (contains '.' or exponent, no base '#').
            if t.contains('.') || t.contains('e') || t.contains('E') {
                return Expr::Real(v);
            }
            self.warn(e.span(), "real literals unsupported; X");
            return Expr::Lit(Value::x(32));
        }
        let _ = env;
        self.err(e.span(), format!("unsupported literal '{}'", t));
        Expr::Lit(Value::x(1))
    }

    /// Parse a VHDL integer literal: decimal (`1_000`) or based (`16#FF#E1`).
    fn parse_int_literal(&mut self, t: &str) -> Option<Value> {
        let t = t.trim();
        if let Some(h) = t.find('#') {
            let base: u32 = t[..h].parse().ok()?;
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

fn is_stmt_node(k: SyntaxKind) -> bool {
    matches!(
        k,
        SyntaxKind::SeqAssign
            | SyntaxKind::VarAssign
            | SyntaxKind::If
            | SyntaxKind::Case
            | SyntaxKind::For
            | SyntaxKind::LoopStmt
            | SyntaxKind::Wait
            | SyntaxKind::NullStmt
            | SyntaxKind::AssertStmt
            | SyntaxKind::ExitStmt
            | SyntaxKind::CallStmt
            | SyntaxKind::ReturnStmt
    )
}

fn bits_for(n: usize) -> usize {
    (usize::BITS - (n.max(1) - 1).leading_zeros()).max(1) as usize
}

/// Case-insensitive "did you mean" suggestion over known unit names: prefers
/// prefix overlap (`edge_detector` → `edge_detector_entity`), then a small
/// edit distance. Exact (case-insensitive) matches are never suggested.
fn did_you_mean<'c>(name: &str, candidates: impl Iterator<Item = &'c String>) -> Option<&'c str> {
    if name.is_empty() {
        return None;
    }
    let n = name.to_ascii_lowercase();
    let mut best: Option<(usize, &str)> = None;
    for c in candidates {
        let cl = c.to_ascii_lowercase();
        if cl == n {
            continue;
        }
        let prefix = cl.starts_with(&n) || n.starts_with(&cl);
        let d = if prefix {
            cl.len().abs_diff(n.len())
        } else {
            levenshtein(&n, &cl)
        };
        if prefix || d <= (n.len() / 3).max(3) {
            if best.map_or(true, |(bd, _)| d < bd) {
                best = Some((d, c.as_str()));
            }
        }
    }
    best.map(|(_, c)| c)
}

/// Levenshtein edit distance, counted in chars.
fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = Vec::with_capacity(b.len() + 1);
        cur.push(i + 1);
        for (j, cb) in b.iter().enumerate() {
            cur.push(
                (prev[j] + usize::from(ca != *cb))
                    .min(prev[j + 1] + 1)
                    .min(cur[j] + 1),
            );
        }
        prev = cur;
    }
    prev[b.len()]
}

/// VHDL time unit → femtoseconds.
/// Collect the expressions of all `return` statements in a lowered body.
fn collect_return_exprs(s: &Stmt, out: &mut Vec<Expr>) {
    match s {
        Stmt::Return(e) => out.push((**e).clone()),
        // `return e` may also lower to a plain assign to the function-name
        // slot (slot 0).
        Stmt::Assign {
            lhs: Expr::Sig(0),
            rhs,
            ..
        } => out.push(rhs.clone()),
        Stmt::Block(ss) => {
            for s in ss {
                collect_return_exprs(s, out);
            }
        }
        Stmt::If { then, els, .. } => {
            collect_return_exprs(then, out);
            if let Some(e) = els {
                collect_return_exprs(e, out);
            }
        }
        Stmt::Case { items, default, .. } => {
            for (_, st) in items {
                collect_return_exprs(st, out);
            }
            if let Some(d) = default {
                collect_return_exprs(d, out);
            }
        }
        Stmt::For { body, .. } | Stmt::Repeat { body, .. } | Stmt::Delay { stmt: body, .. } => {
            collect_return_exprs(body, out)
        }
        Stmt::Spanned { inner, .. } => collect_return_exprs(inner, out),
        _ => {}
    }
}

/// Real-valued conversion (`to_real(x)`, `to_ufixed(x, ...)`, `to_float(x)`,
/// 3-arg `resize`): lowered as `x * 1.0`, which the simulator evaluates as a
/// real-typed (f64) expression. Fixed/float bounds are not modeled.
fn as_real(e: Expr) -> Expr {
    Expr::Bin {
        op: BinOp::Mul,
        l: Box::new(e),
        r: Box::new(Expr::Real(1.0)),
    }
}

fn time_unit_fs(unit: &str) -> u64 {
    match unit {
        "fs" => 1,
        "ps" => 1_000,
        "ns" => 1_000_000,
        "us" | "µs" => 1_000_000_000,
        "ms" => 1_000_000_000_000,
        "sec" | "s" => 1_000_000_000_000_000,
        "min" => 60_000_000_000_000_000,
        "hr" => 3_600_000_000_000_000_000,
        _ => 1_000_000,
    }
}

fn vhdl_binop(op: &str) -> Option<BinOp> {
    Some(match op {
        "+" => BinOp::Add,
        "-" => BinOp::Sub,
        "*" => BinOp::Mul,
        "/" => BinOp::Div,
        "mod" | "rem" => BinOp::Mod,
        "**" => BinOp::Pow,
        "=" => BinOp::Eq,
        "/=" => BinOp::Ne,
        "?=" => BinOp::EqM,
        "?/=" => BinOp::NeM,
        "<" => BinOp::Lt,
        "<=" => BinOp::Le,
        ">" => BinOp::Gt,
        ">=" => BinOp::Ge,
        "?<" => BinOp::LtM,
        "?<=" => BinOp::LeM,
        "?>" => BinOp::GtM,
        "?>=" => BinOp::GeM,
        "and" => BinOp::BAnd,
        "or" => BinOp::BOr,
        "xor" => BinOp::BXor,
        "nand" => BinOp::BNand,
        "nor" => BinOp::BNor,
        "xnor" => BinOp::BXnor,
        "sll" | "sla" => BinOp::Shl,
        "srl" => BinOp::Shr,
        "sra" => BinOp::Sar,
        "rol" => BinOp::Rol,
        "ror" => BinOp::Ror,
        _ => return None,
    })
}

fn vhdl_unop(op: &str) -> Option<UnOp> {
    Some(match op {
        "-" => UnOp::Neg,
        "+" => UnOp::Pos,
        "not" => UnOp::BNot,
        _ => return None,
    })
}

/// Extract edge sensitivities from a wait-until condition: rising_edge(s) →
/// Pos edge on s; anything else → Any edge on every signal in the condition.
fn edges_from_cond(c: &Expr, lx: &Lower) -> Option<Vec<Edge>> {
    match c {
        Expr::Rising(inner) | Expr::Falling(inner) => {
            if let Expr::Sig(s) = &**inner {
                return Some(vec![Edge {
                    sig: *s,
                    kind: if matches!(c, Expr::Rising(_)) {
                        EdgeKind::Pos
                    } else {
                        EdgeKind::Neg
                    },
                }]);
            }
            None
        }
        _ => {
            let mut sigs = Vec::new();
            c.collect_sigs(&mut sigs);
            let _ = lx;
            if sigs.is_empty() {
                None
            } else {
                Some(
                    sigs.iter()
                        .map(|s| Edge {
                            sig: *s,
                            kind: EdgeKind::Any,
                        })
                        .collect(),
                )
            }
        }
    }
}

/// String as a Value (first char in the high byte, matching `lower_literal`).
fn string_value(s: &str) -> Value {
    let bytes = s.as_bytes();
    let n = bytes.len().max(1);
    let mut bits = vec![V0; n * 8];
    for (bi, &b) in bytes.iter().enumerate() {
        for j in 0..8 {
            bits[(n - 1 - bi) * 8 + j] = if (b >> j) & 1 == 1 { V1 } else { V0 };
        }
    }
    Value { bits }
}

/// Build a Value from a u128 with the minimal width that holds it.
fn value_from_u128(width: usize, v: u128) -> Value {
    let mut bits = vec![V0; width.max(1)];
    for i in 0..width.min(128) {
        bits[i] = if (v >> i) & 1 == 1 { V1 } else { V0 };
    }
    Value { bits }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn did_you_mean_prefers_close_names() {
        let cands = vec!["edge_detector_entity".to_string(), "fifo".to_string()];
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
}
