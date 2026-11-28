//! Module-level IR (before elaboration).
//!
//! One module = ports + signals + continuous assigns + processes + functions +
//! instances. Ids are arena indices into the module's vectors. Expressions
//! reference signals by `SigId` within the same module; elaboration
//! ([`crate::elab`]) remaps them into the flat signal space.

use crate::value::{BinOp, UnOp, Value};
use hdl_syntax::source::Span;
use serde::Serialize;
use std::collections::HashMap;

pub use stmt_util::is_bodiless;
mod stmt_util;

/// Field-level serializer for `Stmt::Spanned::span`: `Span`/`FileId` live in
/// `hdl-syntax`, which is deliberately dependency-free — serialize the parts
/// as plain numbers instead of adding serde to that crate.
pub mod serde_span {
    use hdl_syntax::source::Span;
    use serde::{Serialize, Serializer};
    pub fn span<S: Serializer>(sp: &Span, s: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Parts {
            file: u32,
            start: u32,
            end: u32,
        }
        Parts {
            file: sp.file.0,
            start: sp.start,
            end: sp.end,
        }
        .serialize(s)
    }
}

pub type SigId = u32;
pub type ModId = u32;

#[derive(Clone, Debug, Default)]
pub struct Design {
    pub modules: Vec<ModuleIr>,
    /// Name of the top module to elaborate, if chosen.
    pub top: Option<String>,
    /// Lowered packages by unit name (`std_logic_1164`, ...), so other files
    /// can make them visible with `use lib.pkg.all;` (see
    /// [`crate::vhdl_lower`]).
    pub packages: HashMap<String, std::rc::Rc<crate::vhdl_lower::PackageIr>>,
    /// Lowered configuration declarations by unit name, so instantiations
    /// (`label : configuration work.cfg`) and `--top cfg` resolve (see
    /// [`crate::vhdl_lower::ConfigIr`]).
    pub configurations: HashMap<String, std::rc::Rc<crate::vhdl_lower::ConfigIr>>,
    /// Const-eval fallback shared by every file lowering in this design: all
    /// packages' function bodies grouped by lowercase name (the lowerer's
    /// `funcs_ce`). Kept in sync as packages enter `self.packages` (one
    /// clone per function per design), because rebuilding it per lowered
    /// file — which re-clones the whole package closure several times per
    /// file — is quadratic and dominated large library compiles.
    pub pkg_funcs: std::rc::Rc<HashMap<String, Vec<std::rc::Rc<FuncIr>>>>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModuleIr {
    pub name: String,
    pub params: Vec<ParamIr>,
    pub ports: Vec<PortIr>,
    /// All signals of the module, including ports (ports reference their signal).
    pub signals: Vec<SignalIr>,
    pub assigns: Vec<AssignIr>,
    pub processes: Vec<ProcessIr>,
    pub functions: Vec<FuncIr>,
    pub instances: Vec<InstIr>,
    /// Timescale unit in femtoseconds (from `` `timescale ``), default 1 ns.
    pub time_unit_fs: u64,
    /// Source language: VHDL modules can be re-lowered as elaboration
    /// variants (generic-map overrides); Verilog modules cannot.
    pub is_vhdl: bool,
    /// Signal name → inclusive bit ranges of real-typed record fields
    /// (`type c is record re, im : real; end record;` → the whole signal).
    /// The simulator decodes those slices as IEEE-754; everything else as an
    /// integer bit pattern (VHDL records flatten to one vector per signal).
    pub real_ranges: HashMap<String, Vec<(u32, u32)>>,
    /// Debug only (`--dump-maps`): signal id (module-relative) → array
    /// dims [(low, size), …] as believed at the end of lowering this unit.
    /// Not consumed by the kernel; omitted when unset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sig_dims_debug: Option<HashMap<u32, Vec<(i64, usize)>>>,
}

impl Default for ModuleIr {
    fn default() -> Self {
        ModuleIr {
            name: String::new(),
            params: Vec::new(),
            ports: Vec::new(),
            signals: Vec::new(),
            assigns: Vec::new(),
            processes: Vec::new(),
            functions: Vec::new(),
            instances: Vec::new(),
            time_unit_fs: 1_000_000,
            is_vhdl: false,
            real_ranges: HashMap::new(),
            sig_dims_debug: None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ParamIr {
    pub name: String,
    pub default: Value,
    /// Type-mark name of the generic (records: the registered record name;
    /// scalars: the bare mark, e.g. `integer`) — restored into the
    /// architecture scope's `const_types` so a use of the generic recovers
    /// its signedness and record field selects resolve when the
    /// architecture is lowered in a fresh scope (separate file).
    pub ty: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PortIr {
    pub name: String,
    pub dir: Dir,
    pub sig: SigId,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
pub enum Dir {
    In,
    Out,
    Inout,
}

#[derive(Clone, Debug, Serialize)]
pub struct SignalIr {
    pub name: String,
    pub width: usize,
    pub kind: SigKind,
    pub init: Option<Value>,
    /// `Some(depth)` for memories: `reg [7:0] mem [0:255]` → width 8, mem 256.
    pub mem: Option<usize>,
    /// VHDL `signed` types (and Verilog `signed` regs): arithmetic/comparisons
    /// sign-extend instead of zero-extend.
    pub signed: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
pub enum SigKind {
    Wire,
    Reg,
    Input,
    Output,
    Inout,
    /// Loop variable of a generate-for (elaboration-time constant).
    Genvar,
    /// Verilog `real` / VHDL `real`: 64-bit, value is the f64 bit pattern.
    Real,
}

#[derive(Clone, Debug, Serialize)]
pub struct AssignIr {
    pub lhs: Expr,
    pub rhs: Expr,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProcessIr {
    pub kind: ProcKind,
    pub sens: Sens,
    pub body: Stmt,
    /// Human-readable origin for debug/wave display, e.g. "always @(posedge clk)".
    pub origin: String,
    /// Instance-path suffix of the enclosing generate blocks (`lane_gen(0)`,
    /// dotted when nested; VHDL parentheses, Verilog brackets; empty outside
    /// generates). The elaborator appends it to the module's instance prefix
    /// to build `FlatProc::scope`, so the transcript can attribute a report
    /// to the exact unrolled instance.
    pub gen_scope: String,
    /// Run once at t=0 before waiting on `sens`. VHDL processes always do;
    /// Verilog `always @(...)` blocks do not.
    pub initial: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
pub enum ProcKind {
    Always,
    Initial,
}

#[derive(Clone, Debug, Serialize)]
pub enum Sens {
    /// Explicit sensitivity list with edge kinds.
    List(Vec<Edge>),
    /// `@*` — all signals read by the body, computed during lowering.
    All,
}

#[derive(Clone, Debug, Serialize)]
pub struct Edge {
    pub sig: SigId,
    pub kind: EdgeKind,
    /// Verilog `iff` event qualifier (SV LRM 9.4.2.3): the edge fires only
    /// when this condition is true at the edge. `None` = unconditional.
    pub iff: Option<Expr>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
pub enum EdgeKind {
    Pos,
    Neg,
    Any,
}

/// Formal argument mode (LRM 1076 §4.3): `in` (default), `inout`, `out`.
/// Only procedures carry non-`in` formals: on return, a caller's `variable`
/// actual of an `inout`/`out` formal receives the formal's final value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum ArgMode {
    In,
    Inout,
    Out,
}

/// A shape-bound (unconstrained 1-D) formal's call-time index shape. The
/// formal's body is lowered once, but element/slice addressing depends on the
/// actual's range direction and low bound — each call binds them through two
/// hidden frame slots the body's addressing reads (LRM 1076 §5.2: an
/// unconstrained formal takes the actual's index range).
#[derive(Clone, Debug, Serialize)]
pub struct FormalShape {
    /// Position of the formal in [`FuncIr::args`].
    pub formal: usize,
    /// Hidden 32-bit slots: the actual's low bound, and 1 for an ascending
    /// (`to`) actual / 0 for descending (`downto`).
    pub lo: SigId,
    pub asc: SigId,
    /// Binding used when a call node carries no [`ShapeBind`] for this
    /// formal (the call was lowered against an interface-only stub, or the
    /// actual's shape is not statically known): the kernel convention the
    /// formal would otherwise address with — (1, ascending) for `string`,
    /// else (0, descending), matching the pre-shape lowerings bit for bit.
    pub fb_lo: i64,
    pub fb_asc: bool,
}

/// One formal's shape binding on a call node: the actual's low bound and
/// direction as expressions evaluated in the caller's context. Keyed by
/// formal position, not carried in `args`, so overload candidates that need
/// no shape ignore it and every arity/width dispatch check stays untouched.
#[derive(Clone, Debug, Serialize)]
pub struct ShapeBind {
    pub formal: usize,
    pub lo: Expr,
    pub asc: Expr,
}

#[derive(Clone, Debug, Serialize)]
pub struct FuncIr {
    pub name: String,
    pub ret_width: usize,
    /// (name, width, declared type name lowercased) of arguments, in order.
    /// The type is empty when unknown (Verilog, synthesized stubs) — it
    /// disambiguates same-width overloads at dispatch: the three
    /// unconstrained vector formals of `conv_float_phase` (`signed`,
    /// `unsigned`, `std_ulogic_vector`, all w1) are told apart only by this,
    /// and picking wrong one makes the delegating bodies recurse forever.
    pub args: Vec<(String, usize, String)>,
    /// Per-formal mode, parallel to `args`. Functions are always `In`;
    /// Verilog tasks record no directions today and stay `In`.
    pub modes: Vec<ArgMode>,
    /// Shape-bound (unconstrained 1-D) formals: the hidden shape slots their
    /// bodies address through, in slot order. Empty when every formal is
    /// scalar or statically constrained (all Verilog subprograms, e.g.).
    pub hidden: Vec<FormalShape>,
    /// Const-evaluated default value per formal (same order as `args`),
    /// `None` where the formal has no default. A call may omit trailing
    /// actuals whose formals carry `Some`. Empty when no formal has one.
    pub defaults: Vec<Option<Value>>,
    pub locals: Vec<SignalIr>,
    /// Locals whose declared constraint depends on subprogram formals
    /// (`variable s : string(1 to size)`): registration folds them to the
    /// fallback width because formals have no values yet, so const-eval
    /// re-folds the recorded bit-width expression per call and grows the
    /// slot. Empty for locals with static constraints.
    pub dyn_locals: Vec<(SigId, Expr)>,
    pub body: Stmt,
}

#[derive(Clone, Debug, Serialize)]
pub struct InstIr {
    pub module: String,
    pub name: String,
    /// Generic/parameter overrides: `Some(name)` for a named association,
    /// `None` for a positional one (bound to the target's parameters in
    /// declaration order during elaboration — the target's list is not
    /// visible while lowering).
    pub param_overrides: Vec<(Option<String>, Expr)>,
    pub conns: Vec<Conn>,
    /// Configuration bound to this instance (`configuration work.cfg` or a
    /// `for label : comp use configuration ...;` specification); elaboration
    /// applies its block bindings to this instance's subtree.
    pub config: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub enum Conn {
    Named(String, Expr),
    Positional(Expr),
}

#[derive(Clone, Debug, Serialize)]
pub enum Stmt {
    Block(Vec<Stmt>),
    If {
        cond: Expr,
        then: Box<Stmt>,
        els: Option<Box<Stmt>>,
    },
    Case {
        expr: Expr,
        items: Vec<(Vec<Expr>, Stmt)>,
        default: Option<Box<Stmt>>,
        /// VHDL-2008 matching case (`case?`): choice bits of `VDASH` ('-')
        /// are wildcards instead of literal values.
        matching: bool,
    },
    /// Procedural assignment; `nba` = non-blocking (`<=`).
    Assign {
        lhs: Expr,
        rhs: Expr,
        nba: bool,
    },
    /// VHDL delayed signal assignment (`s <= v after t;`): the RHS and the
    /// delay are evaluated when the statement executes, but the signal update
    /// happens at now + delay (femtoseconds) without suspending the process.
    AssignDelayed {
        lhs: Expr,
        rhs: Expr,
        delay: Expr,
    },
    /// `#delay stmt` — delay in femtoseconds (timescale applied at lowering).
    Delay {
        time_fs: u64,
        stmt: Box<Stmt>,
    },
    /// `@(edge...) stmt` — suspend the process until a listed edge occurs.
    Wait {
        sens: Vec<Edge>,
        body: Box<Stmt>,
    },
    /// VHDL `wait until cond [for T]` — park on `sens` edges; resume when
    /// `cond` holds at an edge, or when the timeout expires (femtoseconds,
    /// evaluated once when the wait executes). `cond: None` is a pure edge
    /// wait (`wait until rising_edge(clk)`): the edge kind encodes the check,
    /// no condition re-evaluation.
    WaitUntil {
        cond: Option<Expr>,
        sens: Vec<Edge>,
        timeout: Option<Expr>,
    },
    SysCall {
        name: String,
        args: Vec<Expr>,
        /// Per-formal shape bindings for the callee's unconstrained 1-D
        /// formals (see [`ShapeBind`]); empty when none apply.
        shapes: Vec<ShapeBind>,
    },
    For {
        init: Box<Stmt>,
        cond: Expr,
        step: Box<Stmt>,
        body: Box<Stmt>,
    },
    /// `repeat (count) body` — count is evaluated once when the loop is entered.
    Repeat {
        count: Expr,
        body: Box<Stmt>,
    },
    ExprStmt(Expr),
    /// `return expr;` inside a function — set the return slot and stop the
    /// function body (unlike a plain assign to the function-name slot, which
    /// falls through).
    Return(Box<Expr>),
    /// `break;` — exit the innermost loop.
    Break,
    /// `continue;` — jump to the innermost loop's step/next iteration.
    Continue,
    Null,
    /// Source-span wrapper attached by the lowerers to every statement — used
    /// for line/branch coverage, never changes semantics.
    Spanned {
        #[serde(serialize_with = "serde_span::span")]
        span: Span,
        inner: Box<Stmt>,
    },
}

#[derive(Clone, Debug, Serialize)]
pub enum Expr {
    Lit(Value),
    /// Real (floating-point) literal, e.g. Verilog `1.5`, VHDL `1.5`.
    Real(f64),
    Sig(SigId),
    Bin {
        op: BinOp,
        l: Box<Expr>,
        r: Box<Expr>,
    },
    Un {
        op: UnOp,
        e: Box<Expr>,
    },
    Cond {
        c: Box<Expr>,
        t: Box<Expr>,
        e: Box<Expr>,
    },
    Call {
        name: String,
        args: Vec<Expr>,
        /// Per-formal shape bindings for the callee's unconstrained 1-D
        /// formals (see [`ShapeBind`]); empty when none apply.
        shapes: Vec<ShapeBind>,
    },
    /// `{a, b, ...}` — parts[0] is most significant.
    Concat(Vec<Expr>),
    Replicate {
        count: Box<Expr>,
        e: Box<Expr>,
    },
    /// `e[msb:lsb]`.
    Sel {
        e: Box<Expr>,
        msb: Box<Expr>,
        lsb: Box<Expr>,
    },
    /// `e[i]`.
    Idx {
        e: Box<Expr>,
        i: Box<Expr>,
    },
    /// VHDL `rising_edge(sig)` — true when the signal had a 0→1 edge at the
    /// current simulation time (evaluated against prev/current values).
    Rising(Box<Expr>),
    /// VHDL `falling_edge(sig)`.
    Falling(Box<Expr>),
    /// VHDL `sig'last_value` — the signal's value before the current delta.
    Prev(Box<Expr>),
    /// VHDL `to_integer(x)` — unsigned numeric value as a 32-bit integer.
    ToInt(Box<Expr>),
    /// VHDL `t'image(x)` — value as ASCII bytes; with `boolean`, 1-bit
    /// values spell `true`/`false` (wider operands keep the decimal form).
    ToString {
        e: Box<Expr>,
        boolean: bool,
    },
    /// Resize to a fixed width: `to_unsigned(x, n)` / `resize(x, n)`.
    /// `signed` selects sign-extension (truncation keeps the low bits).
    Resize {
        e: Box<Expr>,
        width: usize,
        signed: bool,
    },
    /// `signed(x)` — pure signedness marker: same bits/width, but signed
    /// semantics for arithmetic and comparisons.
    Signed(Box<Expr>),
    /// `sig'length`/`sig'high`/… where the width is only known after port
    /// connection (unconstrained array ports) — evaluated at runtime from
    /// the (possibly aliased) flat signal.
    SigAttr {
        sig: SigId,
        attr: &'static str,
    },
}

mod sigs;
