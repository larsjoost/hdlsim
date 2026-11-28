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
//! Layout: this file holds `lower_file` and module-level helpers; the public
//! design-unit IR types live in `units`, the `Lower` context struct (its
//! fields are the module's real interface — each submodule's `//!` header
//! states which fields it owns) and its core methods here, and the tests in
//! `tests`. The lowering itself lives in per-responsibility submodules: `context`-free
//! helpers in `scalar`, config/binding in `config`, architecture selection
//! in `variant`, object/record naming in `objects`, type decls in `types`,
//! widths in `width`, entities in `entity`, packages in `package`,
//! architectures/concurrent in `arch`, object decls in `decls`,
//! subprograms in `subprogram` (slot machinery in `slots`), processes in
//! `process`, sequential statements in `seq` (loops/waits in `loops`),
//! selected-name chains in `chains`, aggregates in `aggregate`,
//! expressions/attributes in `expr`/`attrs`/`dump`.

use crate::ir::*;
use crate::levenshtein_bounded;
use crate::value::{BinOp, UnOp, Value, V0, V1, VDASH, VH, VL, VU, VW, VX, VZ};
use crate::verilog_lower::const_eval_f_with_pkgs;
use hdl_syntax::cst::SyntaxNode;
use hdl_syntax::diagnostic::{Diagnostic, Severity};
use hdl_syntax::kinds::SyntaxKind;
use hdl_syntax::source::{FileId, SourceDb, Span};
use serde::Serialize;

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
mod context;
use context::use_clause_units;
pub use context::{
    context_libraries, context_unit_libs, context_units, declared_unit_names,
    package_inst_target_units,
};
mod arch;
mod attrs;
mod chains;
mod char_lits;
mod compare;
mod config;
mod decls;
mod dump;
mod entity;
mod expr;
mod loops;
mod objects;
mod package;
mod passes;
mod proc_vars;
mod process;
mod process_sel;
mod scalar;
mod seq;
mod sigmaps;
pub use sigmaps::{ProtType, SigMaps, TypeInfo};
mod slots;
mod subprogram;
mod types;
mod units;
pub use units::{Binding, BlockCfg, ConfigIr, Lowered, ObjClass, ObjDims, PackageIr, PendingArch};
use units::{ConcOut, VariantSpec, WaveElem};
mod variant;
mod width;
pub(crate) use scalar::*;
use variant::{apply_default_archs, arch_entity};
pub use variant::{lower_entity_variant, lower_pending_architectures, stem_key};

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
    let mut lx = Lower::for_file(root, db, diags, pkgs, pkg_funcs, cfgs);
    let mut modules: Vec<ModuleIr> = Vec::new();
    // Pass 0: packages and package instantiations, before anything that may
    // `use` them (see `lower_pass0_packages`).
    let mut packages = passes::lower_pass0_packages(&mut lx, root);
    // Re-apply the context clauses now that same-file packages (plain and
    // instantiated) are in `pkgs`.
    lx.apply_context_clauses(root);
    // Pass 1: entities (interfaces).
    let (entity_roots, entities) = passes::lower_pass1_entities(&mut lx, root, &mut modules);
    // Configurations first, so `configuration work.cfg` instantiations and
    // `use configuration` bindings resolve regardless of declaration order.
    let (configurations, config_defaults) = passes::lower_config_decls(&mut lx, root);
    // Pass 2: architectures (bodies); ones whose entity is not declared in
    // this file are deferred (`pending`) — see `lower_pass2_architectures`.
    let mut pending: Vec<PendingArch> = Vec::new();
    let last_arch =
        passes::lower_pass2_architectures(&mut lx, db, root, &entities, &mut modules, &mut pending);
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
    /// Signal id → std_logic_arith comparison class (see [`ObjClass`]).
    sig_class: HashMap<SigId, ObjClass>,
    /// std_logic_arith's wrap-semantics mixed comparisons are in scope
    /// (this file's use clauses, or the inherited entity context).
    arith_visible: bool,
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
    /// Constant/generic name → per-dim ascending flags (`obj_asc`), from the
    /// decl constraint or value shape — index/slice addressing remaps.
    obj_asc: HashMap<String, Vec<bool>>,
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
    /// Shape-bound (unconstrained 1-D vector) formal slot id → its hidden
    /// (lo, asc) shape slot ids. The body's element/slice addressing remaps
    /// through them (the formal takes the actual's index range, LRM 1076
    /// §5.2.1); taken and restored with the slot scope — ids alias across
    /// subprograms.
    formal_dyn: HashMap<SigId, (SigId, SigId)>,
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
    /// Delta of `obj_asc` in the entity declarative part, via `SigMaps`
    /// (see `entity_obj_dims`).
    entity_obj_asc: HashMap<String, Vec<bool>>,
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
    /// Enclosing function's return type is string-typed: `RETURN "0"` in a
    /// string-returning function is text, so the return expression lowers in
    /// the string context (set in `lower_fn_into_slots`, consulted by
    /// `lower_return_stmt`).
    fn_ret_str: bool,
    /// Names currently bound to no value whose X roots in an unbound formal
    /// generic (a required formal with no default, and constants derived from
    /// one): expressions referencing them are constant per instance, so their
    /// bare-context fold failure warns misleadingly and is skipped instead.
    per_instance_names: std::collections::HashSet<String>,
    /// Impure function designators in this file (lowercase): `refs_impure_call`.
    impure_fns: std::collections::HashSet<String>,
    /// User-defined attribute names in scope (`attribute a : T;` in this file
    /// or a `use`d package) — `x'a` lowers to X silently for these.
    attr_decls: std::collections::HashSet<String>,
    /// Counter for path-less file declarations: each gets a unique
    /// `$file:name#n` marker constant, resolved at runtime by file_open.
    file_seq: u64,
    /// Generate blocks currently being unrolled, innermost last: one element
    /// per enclosing for-generate, `label(index)` (loop variable fallback
    /// when unlabeled). Processes and instances lowered inside get it as an
    /// instance-path suffix, so two unrolled lanes are distinguishable in the
    /// transcript and in signal names.
    gen_path: Vec<String>,
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
            sig_class: HashMap::new(),
            arith_visible: false,
            sig_rec: HashMap::new(),
            pkg_sig_types: HashMap::new(),
            pkg_sig_widths: HashMap::new(),
            sig_elem_rec: HashMap::new(),
            sig_width: HashMap::new(),
            sig_dims: HashMap::new(),
            obj_dims: HashMap::new(),
            obj_asc: HashMap::new(),
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
            formal_dyn: HashMap::new(),
            aliases: HashMap::new(),
            alias_rec: HashMap::new(),
            suppress_err: 0,
            fn_ret_str: false,
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
            entity_obj_asc: HashMap::new(),
            entity_aliases: HashMap::new(),
            entity_alias_rec: HashMap::new(),
            attr_decls: std::collections::HashSet::new(),
            variant: None,
            cur_entity: String::new(),
            cur_scope: String::new(),
            str_ctx: false,
            per_instance_names: std::collections::HashSet::new(),
            impure_fns: std::collections::HashSet::new(),
            prot_types: HashMap::new(),
            prot_body_ctx: None,
            sig_prot: HashMap::new(),
            pending_funcs: Vec::new(),
            file_seq: 0,
            gen_path: Vec::new(),
        }
    }

    /// A blank lowerer wired for one design file: built-in types registered,
    /// the design-wide package/config tables installed (lowercased keys,
    /// matching the `use`-clause lookups), and the file's context clauses
    /// applied.
    #[allow(clippy::too_many_arguments)]
    fn for_file(
        root: &SyntaxNode,
        db: &'a SourceDb,
        diags: &'a mut Vec<Diagnostic>,
        pkgs: &HashMap<String, Rc<PackageIr>>,
        pkg_funcs: &std::rc::Rc<HashMap<String, Vec<std::rc::Rc<FuncIr>>>>,
        cfgs: &HashMap<String, Rc<ConfigIr>>,
    ) -> Lower<'a> {
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
        lx
    }

    /// Snapshot the current signal maps (called after an entity's ports are
    /// lowered) and clear them so the next entity starts fresh.
    fn snapshot_entity_maps(&mut self, entity: &str) {
        let maps = SigMaps {
            mems: self.mems.clone(),
            sig_signed: self.sig_signed.clone(),
            sig_class: self.sig_class.clone(),
            arith: self.arith_visible,
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
            obj_asc: self.entity_obj_asc.clone(),
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
        self.sig_class.clear();
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
        self.sig_class = m.sig_class;
        // OR, not assign: an arch file's own `use ieee.std_logic_arith`
        // must survive the entity's install (and vice versa).
        self.arith_visible |= m.arith;
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
        for (k, v) in m.obj_asc {
            self.obj_asc.entry(k).or_insert(v);
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
                // Whitespace is free between tokens (`pkg . item` equals
                // `pkg.item`), so every dotted part is trimmed — the raw
                // span text keeps interior spacing (`pkg. item`) that would
                // otherwise miss the item key.
                let parts: Vec<&str> = text.split('.').map(|p| p.trim()).collect();
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
                // std_logic_arith's mixed comparisons wrap the integer
                // operand (numeric_std's extend instead) — recorded for the
                // comparison gate in `wrap_mixed_compare`.
                if unit.eq_ignore_ascii_case("std_logic_arith") {
                    self.arith_visible = true;
                }
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
mod tests;
