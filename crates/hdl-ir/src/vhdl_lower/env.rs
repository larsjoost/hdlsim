//! The `Lower` context type: the state threaded through every VHDL lowering
//! submodule. Its fields are the module's real interface — each submodule's
//! `//!` header states which fields it owns. The blank constructor
//! (`Lower::new`) and the type's core methods stay at the vhdl_lower module
//! root.
//!
//! Extracted verbatim from vhdl_lower.rs (pure move; the move forces
//! `pub(super)` on the type and its fields — they were module-private only
//! because they lived at the module root).

use super::*;

/// The VHDL lowering context: one per design file, carrying the source
/// database, the diagnostics sink, and every scope table the lowering
/// submodules share.
pub(super) struct Lower<'a> {
    pub(super) db: &'a SourceDb,
    pub(super) diags: &'a mut Vec<Diagnostic>,
    /// Compile-time constants in scope (enum literals, constants, generics).
    pub(super) consts: Rc<HashMap<String, Value>>,
    /// Constant name -> type mark, so record/array-typed constants can be
    /// materialized as hidden signals for runtime indexing (`c(i).field`).
    pub(super) const_types: HashMap<String, String>,
    /// User function bodies in scope (package functions via `use`), for
    /// const-evaluating calls in constant initializers.
    /// Lowered functions/procedures by bare name. Overloads (same name,
    /// different signatures) coexist as candidates; `register_func` keeps
    /// the map invariant and const-eval picks by argument fit.
    pub(super) funcs: HashMap<String, Vec<FuncIr>>,
    /// Packages already bulk-merged (`use lib.pkg.all`) into this scope —
    /// merging is transitive, so without this the same package closure is
    /// re-cloned (types/consts/functions) once per use clause per file.
    pub(super) merged_all: std::collections::HashSet<String>,
    /// Const-eval fallback: function bodies from every known package,
    /// shared design-wide (`Design::pkg_funcs`) and only consulted when the
    /// direct `self.funcs` map misses. A use clause may merge only one
    /// function of a package, yet its body may legally call a
    /// package-internal sibling (visible at the body's declaration site), so
    /// nested const-eval resolves against this wider table when the direct
    /// map misses. Cross-file packages only: same-file packages register
    /// their functions into `self.funcs` directly.
    pub(super) funcs_ce: std::rc::Rc<HashMap<String, Vec<std::rc::Rc<FuncIr>>>>,
    /// Type marks in scope (built-ins + user type/subtype decls).
    pub(super) types: HashMap<String, TypeInfo>,
    /// Subtype name -> constrained width, so `signal x : my_subtype` (no
    /// constraint at the use site) keeps the subtype's width.
    pub(super) type_widths: HashMap<String, usize>,
    /// Element width per Vector-subtype name (`string` subtypes: 8) —
    /// `array_elem_width` consults it; exported via
    /// [`PackageIr::type_elem`] for cross-file folds.
    pub(super) type_elem: HashMap<String, usize>,
    /// Subtype name -> constrained range [(low, size), …] for vector
    /// subtypes, so aggregates and `'range`/`'length` see the subtype's
    /// bounds (Array subtypes carry theirs in `TypeInfo::Array.dims`).
    pub(super) type_dims: HashMap<String, Vec<(i64, usize)>>,
    /// Signal/variable name → signal id in the current module.
    pub(super) names: HashMap<String, SigId>,
    /// Signal id → memory depth (for `name'range` loops over arrays).
    pub(super) mems: HashMap<SigId, usize>,
    /// Signal id → signed flag (for `resize`/`to_signed` context).
    pub(super) sig_signed: HashMap<SigId, bool>,
    /// Signal id → std_logic_arith comparison class (see [`ObjClass`]).
    pub(super) sig_class: HashMap<SigId, ObjClass>,
    /// std_logic_arith's wrap-semantics mixed comparisons are in scope
    /// (this file's use clauses, or the inherited entity context).
    pub(super) arith_visible: bool,
    /// Signal id → record type name (for `sig.field` selects), if record-typed.
    pub(super) sig_rec: HashMap<SigId, String>,
    /// (record type, field name) → effective element dims [(low, size), …]
    /// for fields whose type is an unconstrained array with a decl-site
    /// constraint (`leaves : leaf_vec_t(0 to 3)`) — the chain walker needs
    /// the low bound to flatten indexes over the field's slice.
    pub(super) rec_field_dims: HashMap<(String, String), Vec<(i64, usize)>>,
    /// Package-level signal / shared variable name → type mark, so that
    /// `name.field` (record field) and `name.method(...)` (protected type)
    /// resolve when lowering package bodies.
    pub(super) pkg_sig_types: HashMap<String, String>,
    /// Package-level signal widths (name → width) from merged packages.
    pub(super) pkg_sig_widths: HashMap<String, usize>,
    /// Signal id → element record type name for arrays of records, so
    /// `arr(i).field` lowers to a slice of the memory element.
    pub(super) sig_elem_rec: HashMap<SigId, String>,
    /// Signal id → width in bits (for `'length`/`'high`/… attributes).
    pub(super) sig_width: HashMap<SigId, usize>,
    /// Signal id → array dimensions [(low, size), …] (for `m(i, j)` flat index).
    pub(super) sig_dims: HashMap<SigId, Vec<(i64, usize)>>,
    /// Constant/generic name → array dimensions [(low, size), …] from its
    /// decl constraint, default, or aggregate actual — lets `'length(d)` /
    /// `'high(d)` fold on multidimensional constants and generics (the
    /// object constraint is not part of the type mark, so `types` alone
    /// only knows the unconstrained base's dims).
    pub(super) obj_dims: ObjDims,
    /// Constant/generic name → per-dim ascending flags (`obj_asc`), from the
    /// decl constraint or value shape — index/slice addressing remaps.
    pub(super) obj_asc: HashMap<String, Vec<bool>>,
    /// Signal id → (low bound, ascending?) of a vector's own index
    /// constraint — ascending ranges (`0 to N-1`) address flat bits
    /// mirrored (element k = value bit W-1-k), so slices must remap.
    pub(super) sig_asc: HashMap<SigId, (i64, bool)>,
    /// Signal id → element width in bits for multi-bit-element vectors
    /// (`string`: 8-bit characters) — index/slice bounds are element
    /// positions, not bit positions, so addressing must scale by this.
    /// Absent = 1-bit elements (plain vectors).
    pub(super) sig_elem: HashMap<SigId, usize>,
    /// Signal id → bit-width expression for function locals whose
    /// constraint hangs on a subprogram formal (`string(1 to size)`) —
    /// index/slice remaps must use the per-call width, not the recorded
    /// fallback (`FuncIr.dyn_locals` is the const-eval twin).
    pub(super) sig_dynw: HashMap<SigId, Expr>,
    /// Total bits of the assignment/initializer target while its aggregate
    /// expression lowers, when the target is a string (8-bit elements): an
    /// `(others => ' ')` replicate must cover the whole target, and without
    /// the target context it baked a fixed 32-bit count. `None` outside
    /// string-target aggregate lowering keeps every other aggregate at its
    /// previous behavior.
    pub(super) agg_target_w: Option<usize>,
    /// Access type name → designated type name, so `p.field` on an
    /// access-to-record lowers as an implicit `.all` dereference.
    pub(super) access_inner: HashMap<String, String>,
    /// (record type, field) → field type mark, for nested record selects
    /// (`rv.param.distribution`).
    pub(super) rec_field_ty: HashMap<(String, String), String>,
    /// User-declared physical type unit name -> value in the type's base
    /// unit (`units angstrom; nm = 10 angstrom; ...` -- `nm` -> 10). Lets
    /// physical literals (`1.5 cm`) and bare unit names (`mm`) lower to a
    /// base-unit integer instead of erroring.
    pub(super) phys_units: HashMap<String, u64>,
    /// Hidden signals allocated while lowering statements (for-loop vars),
    /// appended to the module after the process body is lowered.
    pub(super) extra_signals: Vec<SignalIr>,
    /// `module.signals.len()` captured when statement lowering starts.
    pub(super) extra_base: usize,
    /// Enclosing function scopes while lowering a nested function body, so a
    /// reference to an outer parameter/local (`buf.all` inside a nested
    /// function) resolves instead of erroring.
    pub(super) names_stack: Vec<HashMap<String, SigId>>,
    /// Counter for auto-naming instances (the parser drops statement labels,
    /// so `InstIr.name` is always empty; elab needs unique prefixes).
    pub(super) inst_count: usize,
    /// Ports declared without a constraint (`port (a : in std_ulogic_vector)`)
    /// — their width is only known after port connection, so static
    /// attributes on them lower to runtime `Expr::SigAttr`.
    pub(super) uncon_ports: std::collections::HashSet<SigId>,
    /// Slot ids of the subprogram formals being lowered — their sizes come
    /// from the call's actuals, so `'length`/`'high` must fold symbolically
    /// even when a previously lowered subprogram left a stale `sig_width`/
    /// `sig_dims` entry for the same slot id (slot ids alias across
    /// subprograms).
    pub(super) fn_formals: std::collections::HashSet<SigId>,
    /// Shape-bound (unconstrained 1-D vector) formal slot id → its hidden
    /// (lo, asc) shape slot ids. The body's element/slice addressing remaps
    /// through them (the formal takes the actual's index range, LRM 1076
    /// §5.2.1); taken and restored with the slot scope — ids alias across
    /// subprograms.
    pub(super) formal_dyn: HashMap<SigId, (SigId, SigId)>,
    /// `alias name is <expr>;` — name → lowered expression (slice/index
    /// aliases can't be a plain SigId). Whole-signal aliases are additionally
    /// inserted into `names`.
    pub(super) aliases: HashMap<String, Expr>,
    /// Record type of a record alias (`alias LAW : R'subtype is s.field;`), so
    /// `LAW.field` selects slice the aliased expression.
    pub(super) alias_rec: HashMap<String, String>,
    /// >0 while lowering binding-actual expressions in configuration
    /// > specifications: actuals name signals of the enclosing architecture,
    /// > which are not in scope design-wide, so E0401s are suppressed.
    pub(super) suppress_err: u32,
    /// Depth of function-body lowering. Inside a body, `names` holds exactly
    /// the formals/locals (slot map), so a formal must shadow an outer
    /// constant that reached the lowering env snapshot (an architecture
    /// constant `C_DATA` colliding with a formal `C_DATA` resolves to the
    /// formal — LRM 10.3 inner visibility).
    pub(super) fn_scope: usize,
    /// All packages visible while lowering this file (library packages from
    /// other files plus same-file ones), keyed by lowercased unit name —
    /// backs `use lib.pkg...;` inside declarative parts and package
    /// instantiations.
    pub(super) pkgs: HashMap<String, Rc<PackageIr>>,
    /// Configurations visible while lowering this file (design-wide, from
    /// other files plus same-file ones) — backs `configuration work.cfg`
    /// instantiations and `use configuration` binding indications.
    pub(super) cfgs: HashMap<String, Rc<ConfigIr>>,
    /// The design file currently being lowered (for re-applying its context
    /// clauses when re-lowering a generic package at an instantiation site).
    pub(super) file_root: Option<SyntaxNode>,
    /// Packages instantiated inside declarative parts (architecture/process/
    /// generate/package bodies) — returned with the file so they register on
    /// the design (their functions are needed at runtime).
    pub(super) inst_packages: Vec<PackageIr>,
    /// Per-entity snapshot of the signal-id → metadata maps. `SigId` is
    /// module-relative, so with several entities per file the maps would
    /// otherwise leak across entities; the architecture of entity E installs
    /// E's snapshot (see lower_architecture).
    pub(super) entity_maps: HashMap<String, SigMaps>,
    /// Same snapshots keyed by (source directory, lowercase entity name).
    /// Two libraries (or two directories of a whole-tree library) may declare
    /// same-named entities with different ports; the name-keyed map alone is
    /// first-wins and would install the wrong entity's scope into a pending
    /// architecture. The dir-keyed entry disambiguates siblings.
    pub(super) entity_maps_by_dir: HashMap<(String, String), SigMaps>,
    /// Delta of `consts`/`types` declared in the entity declarative part,
    /// exported to the entity's architectures via `SigMaps`.
    pub(super) entity_consts: HashMap<String, Value>,
    pub(super) entity_types: HashMap<String, TypeInfo>,
    /// Delta of `obj_dims` recorded in the entity declarative part (generics
    /// and entity constants), exported to the entity's architectures via
    /// `SigMaps` — per-file `obj_dims` maps don't cross files, so a generic's
    /// `'length(d)` in the architecture needs this handoff.
    pub(super) entity_obj_dims: ObjDims,
    /// Delta of `obj_asc` in the entity declarative part, via `SigMaps`
    /// (see `entity_obj_dims`).
    pub(super) entity_obj_asc: HashMap<String, Vec<bool>>,
    /// Delta of `aliases`/`alias_rec` declared in the entity declarative part,
    /// exported to the entity's architectures via `SigMaps` (entity-level
    /// `alias`/`signal` declarations are visible in every architecture of the
    /// entity, LRM 1076 §1.2 — the entity's declarative region encompasses
    /// its architectures).
    pub(super) entity_aliases: HashMap<String, Expr>,
    pub(super) entity_alias_rec: HashMap<String, String>,
    /// Set when re-lowering one entity as an elaboration variant (generic-map
    /// overrides); modules get the `<entity><suffix>` name.
    pub(super) variant: Option<VariantSpec>,
    /// Protected types (VHDL-2008): type name → state fields + methods.
    pub(super) prot_types: HashMap<String, ProtType>,
    /// Protected type whose body is currently being lowered (bare state-var
    /// and method references resolve against it).
    pub(super) prot_body_ctx: Option<String>,
    /// Signal id → protected type name (objects holding a 64-bit handle).
    pub(super) sig_prot: HashMap<SigId, String>,
    /// FuncIrs lowered inside protected bodies / ctor — drained into the
    /// enclosing package's or module's function list.
    pub(super) pending_funcs: Vec<FuncIr>,
    /// Entity currently being lowered (variant override matching).
    pub(super) cur_entity: String,
    /// Scope label for `--dump-consts` records: the package name, or the
    /// `entity` / `entity/arch` module currently being lowered.
    pub(super) cur_scope: String,
    /// String-typed object context: string literals in the initializer of a
    /// `string` (or string-subtype) object are ASCII text, not bit strings —
    /// an all-alphabet literal like `"0x0000"` must lower 8 bits/char, or its
    /// `x` characters X-poison the value and kill `'length` downstream. Set
    /// around initializer lowering only (see `string_typed`).
    pub(super) str_ctx: bool,
    /// Enclosing function's return type is string-typed: `RETURN "0"` in a
    /// string-returning function is text, so the return expression lowers in
    /// the string context (set in `lower_fn_into_slots`, consulted by
    /// `lower_return_stmt`).
    pub(super) fn_ret_str: bool,
    /// Names currently bound to no value whose X roots in an unbound formal
    /// generic (a required formal with no default, and constants derived from
    /// one): expressions referencing them are constant per instance, so their
    /// bare-context fold failure warns misleadingly and is skipped instead.
    pub(super) per_instance_names: std::collections::HashSet<String>,
    /// Impure function designators in this file (lowercase): `refs_impure_call`.
    pub(super) impure_fns: std::collections::HashSet<String>,
    /// User-defined attribute names in scope (`attribute a : T;` in this file
    /// or a `use`d package) — `x'a` lowers to X silently for these.
    pub(super) attr_decls: std::collections::HashSet<String>,
    /// Counter for path-less file declarations: each gets a unique
    /// `$file:name#n` marker constant, resolved at runtime by file_open.
    pub(super) file_seq: u64,
    /// Generate blocks currently being unrolled, innermost last: one element
    /// per enclosing for-generate, `label(index)` (loop variable fallback
    /// when unlabeled). Processes and instances lowered inside get it as an
    /// instance-path suffix, so two unrolled lanes are distinguishable in the
    /// transcript and in signal names.
    pub(super) gen_path: Vec<String>,
}
