//! Module-level IR (before elaboration).
//!
//! One module = ports + signals + continuous assigns + processes + functions +
//! instances. Ids are arena indices into the module's vectors. Expressions
//! reference signals by `SigId` within the same module; elaboration
//! ([`crate::elab`]) remaps them into the flat signal space.

use crate::value::{BinOp, UnOp, Value};
use hdl_syntax::source::Span;
use std::collections::HashMap;

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
    pub packages: HashMap<String, crate::vhdl_lower::PackageIr>,
}

#[derive(Clone, Debug)]
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
}

#[derive(Clone, Debug)]
pub struct ParamIr {
    pub name: String,
    pub default: Value,
}

#[derive(Clone, Debug)]
pub struct PortIr {
    pub name: String,
    pub dir: Dir,
    pub sig: SigId,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dir {
    In,
    Out,
    Inout,
}

#[derive(Clone, Debug)]
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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

#[derive(Clone, Debug)]
pub struct AssignIr {
    pub lhs: Expr,
    pub rhs: Expr,
}

#[derive(Clone, Debug)]
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProcKind {
    Always,
    Initial,
}

#[derive(Clone, Debug)]
pub enum Sens {
    /// Explicit sensitivity list with edge kinds.
    List(Vec<Edge>),
    /// `@*` — all signals read by the body, computed during lowering.
    All,
}

#[derive(Clone, Debug)]
pub struct Edge {
    pub sig: SigId,
    pub kind: EdgeKind,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EdgeKind {
    Pos,
    Neg,
    Any,
}

#[derive(Clone, Debug)]
pub struct FuncIr {
    pub name: String,
    pub ret_width: usize,
    /// (name, width) of arguments, in order.
    pub args: Vec<(String, usize)>,
    pub locals: Vec<SignalIr>,
    pub body: Stmt,
}

#[derive(Clone, Debug)]
pub struct InstIr {
    pub module: String,
    pub name: String,
    pub param_overrides: Vec<(String, Expr)>,
    pub conns: Vec<Conn>,
}

#[derive(Clone, Debug)]
pub enum Conn {
    Named(String, Expr),
    Positional(Expr),
}

#[derive(Clone, Debug)]
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
        span: Span,
        inner: Box<Stmt>,
    },
}

#[derive(Clone, Debug)]
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
}

impl Expr {
    /// All signal ids referenced by this expression (for `@*` sensitivity).
    pub fn collect_sigs(&self, out: &mut Vec<SigId>) {
        match self {
            Expr::Lit(_) | Expr::Real(_) => {}
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
            Expr::Rising(e) | Expr::Falling(e) | Expr::ToInt(e) | Expr::ToString(e) => {
                e.collect_sigs(out)
            }
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
            Stmt::Delay { stmt, .. } => stmt.collect_sigs(out),
            Stmt::Wait { sens, body } => {
                for e in sens {
                    out.push(e.sig);
                }
                body.collect_sigs(out);
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
