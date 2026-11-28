//! Per-module signal metadata snapshot: the maps an architecture inherits
//! from its entity (widths, dims, arith classes, entity-declarative
//! constants, types, and aliases), captured at entity-lowering end and
//! installed while lowering the architecture. `SigId` is relative to one
//! module.

use super::*;

/// Per-module signal metadata (`SigId` is relative to one module).
#[derive(Default, Clone)]
pub struct SigMaps {
    pub(super) mems: HashMap<SigId, usize>,
    pub(super) sig_signed: HashMap<SigId, bool>,
    /// [`ObjClass`] per signal — see [`ObjClass`].
    pub(super) sig_class: HashMap<SigId, ObjClass>,
    /// std_logic_arith was use-visible when the entity was lowered — its
    /// mixed comparisons wrap the integer operand (numeric_std extends).
    pub(super) arith: bool,
    pub(super) sig_rec: HashMap<SigId, String>,
    pub(super) sig_elem_rec: HashMap<SigId, String>,
    pub(super) sig_width: HashMap<SigId, usize>,
    pub(super) sig_dims: HashMap<SigId, Vec<(i64, usize)>>,
    pub(super) sig_asc: HashMap<SigId, (i64, bool)>,
    /// Element width in bits for multi-bit-element vectors (`string`: 8) —
    /// see [`Lower::sig_elem`]. Mirrored with the entity's other signal maps
    /// so an architecture addresses entity-declared strings correctly.
    pub(super) sig_elem: HashMap<SigId, usize>,
    pub(super) uncon_ports: std::collections::HashSet<SigId>,
    /// Constants and types declared in the entity's declarative part — an
    /// architecture sees them (the entity's declarative region encompasses
    /// its architectures, LRM 1076 §1.2).
    pub(super) consts: HashMap<String, Value>,
    pub(super) types: HashMap<String, TypeInfo>,
    /// `obj_dims` recorded in the entity's declarative part (generic and
    /// constant shapes) — the architecture consults them for
    /// `'length`-family folds on those names.
    pub(super) obj_dims: ObjDims,
    /// `obj_asc` of the entity declarative part (see [`Lower::obj_asc`]).
    pub(super) obj_asc: HashMap<String, Vec<bool>>,
    /// Aliases (and their record types) declared in the entity's declarative
    /// part — visible in its architectures too. `SigId`s are module-relative
    /// and the architecture reuses the entity module's signal table, so the
    /// ids need no remapping.
    pub(super) aliases: HashMap<String, Expr>,
    pub(super) alias_rec: HashMap<String, String>,
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
            "sig_class": self.sig_class,
            "uncon_ports": self.uncon_ports,
            "consts": self.consts,
        })
    }
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
