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
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ParamIr {
    pub name: String,
    pub default: Value,
    /// Record type name of a record-typed generic — restored into the
    /// architecture scope so `G_PARAM.field` selects resolve when the
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
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
pub enum EdgeKind {
    Pos,
    Neg,
    Any,
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
    /// Const-evaluated default value per formal (same order as `args`),
    /// `None` where the formal has no default. A call may omit trailing
    /// actuals whose formals carry `Some`. Empty when no formal has one.
    pub defaults: Vec<Option<Value>>,
    pub locals: Vec<SignalIr>,
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
    /// VHDL `t'image(x)` — decimal string of the value, as ASCII bytes.
    ToString(Box<Expr>),
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

impl Expr {
    /// All signal ids referenced by this expression (for `@*` sensitivity).
    pub fn collect_sigs(&self, out: &mut Vec<SigId>) {
        match self {
            Expr::Lit(_) | Expr::Real(_) => {}
            Expr::SigAttr { sig, .. } => out.push(*sig),
            Expr::Sig(s) => out.push(*s),
            Expr::Bin { l, r, .. } => {
                l.collect_sigs(out);
                r.collect_sigs(out);
            }
            Expr::Un { e, .. } => e.collect_sigs(out),
            Expr::Cond { c, t, e } => {
                c.collect_sigs(out);
                t.collect_sigs(out);
                e.collect_sigs(out);
            }
            Expr::Call { args, .. } => {
                for a in args {
                    a.collect_sigs(out);
                }
            }
            Expr::Concat(parts) => {
                for p in parts {
                    p.collect_sigs(out);
                }
            }
            Expr::Replicate { count, e } => {
                count.collect_sigs(out);
                e.collect_sigs(out);
            }
            Expr::Sel { e, msb, lsb } => {
                e.collect_sigs(out);
                msb.collect_sigs(out);
                lsb.collect_sigs(out);
            }
            Expr::Idx { e, i } => {
                e.collect_sigs(out);
                i.collect_sigs(out);
            }
            Expr::Rising(e)
            | Expr::Falling(e)
            | Expr::Prev(e)
            | Expr::ToInt(e)
            | Expr::ToString(e) => e.collect_sigs(out),
            Expr::Resize { e, .. } => e.collect_sigs(out),
            Expr::Signed(e) => e.collect_sigs(out),
        }
    }
}

impl Stmt {
    pub fn collect_sigs(&self, out: &mut Vec<SigId>) {
        match self {
            Stmt::Spanned { inner, .. } => inner.collect_sigs(out),
            Stmt::Return(e) => e.collect_sigs(out),
            Stmt::Block(ss) => {
                for s in ss {
                    s.collect_sigs(out);
                }
            }
            Stmt::If { cond, then, els } => {
                cond.collect_sigs(out);
                then.collect_sigs(out);
                if let Some(e) = els {
                    e.collect_sigs(out);
                }
            }
            Stmt::Case {
                expr,
                items,
                default,
                ..
            } => {
                expr.collect_sigs(out);
                for (conds, body) in items {
                    for c in conds {
                        c.collect_sigs(out);
                    }
                    body.collect_sigs(out);
                }
                if let Some(d) = default {
                    d.collect_sigs(out);
                }
            }
            Stmt::Assign { lhs, rhs, .. } => {
                lhs.collect_sigs(out);
                rhs.collect_sigs(out);
            }
            Stmt::AssignDelayed { lhs, rhs, delay } => {
                lhs.collect_sigs(out);
                rhs.collect_sigs(out);
                delay.collect_sigs(out);
            }
            Stmt::Delay { stmt, .. } => stmt.collect_sigs(out),
            Stmt::Wait { sens, body } => {
                for e in sens {
                    out.push(e.sig);
                }
                body.collect_sigs(out);
            }
            Stmt::WaitUntil {
                cond,
                sens,
                timeout,
            } => {
                for e in sens {
                    out.push(e.sig);
                }
                if let Some(c) = cond {
                    c.collect_sigs(out);
                }
                if let Some(t) = timeout {
                    t.collect_sigs(out);
                }
            }
            Stmt::SysCall { args, .. } => {
                for a in args {
                    a.collect_sigs(out);
                }
            }
            Stmt::For {
                init,
                cond,
                step,
                body,
            } => {
                init.collect_sigs(out);
                cond.collect_sigs(out);
                step.collect_sigs(out);
                body.collect_sigs(out);
            }
            Stmt::Repeat { count, body } => {
                count.collect_sigs(out);
                body.collect_sigs(out);
            }
            Stmt::Break | Stmt::Continue => {}
            Stmt::ExprStmt(e) => e.collect_sigs(out),
            Stmt::Null => {}
        }
    }
}
