//! Design-unit IR types produced by VHDL lowering: the per-file `Lowered`
//! result, packages (`PackageIr`), configurations (`ConfigIr`, `BlockCfg`,
//! `Binding`), deferred architectures (`PendingArch`), and the small shared
//! types the lowering submodules pass around (`VariantSpec`, `WaveElem`,
//! `ConcOut`, `ObjClass`, `RecFieldDims`).
//!
//! Extracted verbatim from vhdl_lower.rs (pure move, no logic changes).

use super::*;

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
pub(super) struct VariantSpec {
    pub(super) entity: String,
    pub(super) suffix: String,
    pub(super) overrides: HashMap<String, Value>,
}

/// Record field index dims: `(record, field) -> [(low bound, size)]` for
/// fields of multi-bit-element array types. Shared by the lowerer's map and
/// the package export that carries it across files.
pub(super) type RecFieldDims = Vec<(String, String, Vec<(i64, usize)>)>;

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
pub(super) struct WaveElem {
    pub(super) val: SyntaxNode,
    pub(super) delay: Option<SyntaxNode>,
    pub(super) cond: Option<SyntaxNode>,
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

/// Result of lowering a concurrent signal assignment: either a continuous
/// assign or (for delayed waveforms) a one-shot process.
pub(crate) enum ConcOut {
    Assign(AssignIr),
    Proc(ProcessIr),
}

/// std_logic_arith comparison class of a declared object: the two classes
/// that drive mixed-comparison lowering (`unsigned`/`signed` arrays wrap the
/// integer operand of a comparison to their own length — the package's
/// comparison overloads convert with `conv_signed`/`conv_unsigned` to
/// `L'LENGTH` — while plain vectors, records etc. keep the extend-to-wider
/// comparison).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize)]
pub enum ObjClass {
    /// `unsigned` or `signed` array object.
    Arith,
    /// Integer scalar object (`integer`/`natural`/`positive`).
    Integer,
    /// Everything else: no mixed-comparison rewriting.
    #[default]
    Other,
}
