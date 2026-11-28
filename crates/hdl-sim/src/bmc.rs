//! Bounded model checking over the elaborated IR (`hdl_ir::elab::Flat`), backed
//! by the own CDCL solver in [`crate::sat`]. Powers `hdlsim prove`.
//!
//! Model: each step t in `0..=steps` is one clock edge — every edge-triggered
//! (`Sens::List`) process fires once per step; `always @*` processes and
//! continuous assigns are combinational and re-evaluated per step. Top-level
//! input ports and undriven signals are free inputs (fresh CNF vars per step).
//! State signals are the ones written by clocked processes or initial blocks;
//! step 0 is constrained by declaration init values / initial blocks (otherwise
//! the initial state is nondeterministic — sound for safety).
//!
//! Properties: `assert property (@(posedge clk) P)` / `ante |-> cons` /
//! `ante |=> cons` (Verilog SVA-lite) and VHDL `assert cond` inside processes
//! lower to `sva_assert`/`assert` system calls; they are extracted into
//! `Property { guards, check, delay }` and a violation at step t is
//! `(∧ guards(t)) ∧ ¬check(t + delay)`. The BMC formula asserts the OR of all
//! violations; SAT gives a counterexample trace, UNSAT means "no violation
//! within `steps` steps".
//!
//! 2-valued encoding (no X/Z): X/Z literal bits encode as 0, `rising_edge` is
//! constant true and `falling_edge` constant false (every step is an edge),
//! division by zero yields 0. Supported statements: blocks, if/case, blocking
//! and non-blocking assigns (incl. part-select/concat LHS), for/repeat with
//! constant trip counts, user function calls (inlined). Unsupported (clear
//! `Err`, never a wrong answer): memories, non-constant indices/selects,
//! `#delay`/`@wait` in executed processes, system tasks other than the property
//! asserts, dynamic loop bounds.
//!
//! Invariants:
//! - Every signal is classified exactly once as state, combinational, or input.
//! - Combinational drivers form an acyclic graph (checked; cycles are an error).
//! - A signal has at most one clocked driver and at most one comb driver.
//! - Never panics on design content; all failures are `Err(String)`.

use crate::sat::{is_neg, neg, var_of, Lit};
use hdl_ir::elab::{Flat, FlatSigId};
use hdl_ir::value::{BinOp, UnOp, Value, V1};
use hdl_ir::{Expr, ShapeBind, SigKind, Stmt};
use std::collections::{HashMap, HashSet};
mod bitvec;
mod builder;
mod classify;
mod expr_util;
mod stmt_scan;

use bitvec::{
    add_bits, and_bits, and_vec, divmod_bits, eq_bits, extend, ge_bits, lt_bits, mul_bits, mux_vec,
    not_bits, or_bits, or_vec, resize_bits, shift_bits, sub_bits, xor_vec, ShiftDir,
};
use builder::Builder;
use classify::{classify, Class, ProcCls, Writer};

use self::expr_util::{expr_has_dash, sig_root};

/// A safety property extracted from an `assert property` / `assert` process.
#[derive(Clone, Debug)]
pub struct Property {
    /// Antecedent guards (conjunction), evaluated at the trigger step.
    pub guards: Vec<Expr>,
    /// The checked condition.
    pub check: Expr,
    /// `|=>` adds one step of delay (checked at trigger + delay).
    pub delay: usize,
    /// Human-readable origin for reports.
    pub origin: String,
}

/// Outcome of a BMC run.
#[derive(Clone, Debug)]
pub enum ProveStatus {
    /// No property violated within the bound (not a proof of safety).
    Safe,
    /// Property index (into [`ProveResult::properties`]) fails at step.
    Violation(usize, usize),
}

/// Result of [`prove`].
#[derive(Clone, Debug)]
pub struct ProveResult {
    pub status: ProveStatus,
    pub steps: usize,
    /// One origin string per property.
    pub properties: Vec<String>,
    /// Counterexample (only on violation): per step, `(signal name, value)`
    /// for all input and state signals, sorted by name.
    pub trace: Vec<Vec<(String, u64)>>,
}

/// Run bounded model checking on an elaborated design for `steps` clock edges.
pub fn prove(flat: &Flat, steps: usize) -> Result<ProveResult, String> {
    let cls = classify(flat)?;
    let props = extract_properties(flat, &cls)?;
    if props.is_empty() {
        return Err(
            "prove: no `assert property` / `assert` found in the design to check".to_string(),
        );
    }
    let mut enc = Encoder::new(flat, cls, steps)?;
    for t in 0..=steps {
        enc.encode_comb(t)?;
        if t == 0 {
            enc.encode_init()?;
        }
        if t < steps {
            enc.encode_transition(t)?;
        }
    }
    // Violation literals per (property, trigger step).
    let mut violations: Vec<(usize, usize, Lit)> = Vec::new();
    for (pi, p) in props.iter().enumerate() {
        let last = steps.saturating_sub(p.delay);
        for t in 0..=last {
            let g = {
                let mut ex = Exec::at(&mut enc, t);
                let mut g = ex.enc.b.true_lit();
                for guard in &p.guards {
                    let v = ex.eval(guard)?;
                    let gt = or_bits(&mut ex.enc.b, &v.bits);
                    g = ex.enc.b.and_lit(g, gt);
                }
                g
            };
            let chk_bits = {
                let mut ex = Exec::at(&mut enc, t + p.delay);
                ex.eval(&p.check)?.bits
            };
            let bad = {
                let chk_t = or_bits(&mut enc.b, &chk_bits);
                enc.b.and_lit(g, neg(chk_t))
            };
            violations.push((pi, t, bad));
        }
    }
    let mut bad = enc.b.false_lit();
    for (_, _, v) in &violations {
        bad = enc.b.or_lit(bad, *v);
    }
    enc.b.clause(&[bad]);
    let properties: Vec<String> = props.iter().map(|p| p.origin.clone()).collect();
    let model = match enc.b.s.solve() {
        None => {
            return Ok(ProveResult {
                status: ProveStatus::Safe,
                steps,
                properties,
                trace: Vec::new(),
            });
        }
        Some(m) => m,
    };
    let lit_val = |l: Lit| -> bool { model[var_of(l)] != is_neg(l) };
    let mut violated = (0usize, 0usize);
    for (pi, t, v) in &violations {
        if lit_val(*v) {
            violated = (*pi, *t);
            break;
        }
    }
    // Counterexample trace over inputs + state.
    let mut traced: Vec<FlatSigId> = enc.cls.input.clone();
    traced.extend(enc.cls.state.iter().copied());
    traced.sort_by_key(|s| enc.flat.signals[*s as usize].name.clone());
    let mut trace = Vec::with_capacity(steps + 1);
    for t in 0..=steps {
        let mut row: Vec<(String, u64)> = Vec::new();
        for s in &traced {
            let name = enc.flat.signals[*s as usize].name.clone();
            let mut v: u64 = 0;
            for (i, &l) in enc.lits[t][*s as usize].iter().enumerate() {
                if i < 64 && lit_val(l) {
                    v |= 1 << i;
                }
            }
            row.push((name, v));
        }
        trace.push(row);
    }
    Ok(ProveResult {
        status: ProveStatus::Violation(violated.0, violated.1),
        steps,
        properties,
        trace,
    })
}

/// Wrap `top` in a synthetic module `__hdlsim_prove_top` whose ports are
/// fresh top-level wires (they become the free BMC inputs). After calling
/// this, elaborate `__hdlsim_prove_top` (with a [`hdl_ir::elab::UnitResolver`]
/// if libraries are involved) and run [`prove`] on the result.
pub fn wrap_top(design: &mut hdl_ir::Design, top: &str) -> Result<(), String> {
    use hdl_ir::ir::{Conn, InstIr, ModuleIr, SignalIr};
    let mi = design
        .modules
        .iter()
        .position(|m| m.name == top)
        .ok_or_else(|| format!("prove: top module '{}' not found", top))?;
    let m = &design.modules[mi];
    let mut signals: Vec<SignalIr> = Vec::new();
    let mut conns: Vec<Conn> = Vec::new();
    for port in &m.ports {
        let ps = &m.signals[port.sig as usize];
        signals.push(SignalIr {
            name: port.name.clone(),
            width: ps.width,
            kind: SigKind::Wire,
            init: None,
            mem: None,
            signed: ps.signed,
        });
        conns.push(Conn::Named(
            port.name.clone(),
            Expr::Sig((signals.len() - 1) as u32),
        ));
    }
    let wrapper = ModuleIr {
        name: "__hdlsim_prove_top".to_string(),
        params: Vec::new(),
        ports: Vec::new(),
        signals,
        assigns: Vec::new(),
        processes: Vec::new(),
        functions: Vec::new(),
        instances: vec![InstIr {
            module: top.to_string(),
            name: "dut".to_string(),
            param_overrides: Vec::new(),
            conns,
            config: None,
        }],
        time_unit_fs: m.time_unit_fs,
        is_vhdl: m.is_vhdl,
        // The DUT's own real-field layout drives its signals' flat ranges.
        real_ranges: m.real_ranges.clone(),
        sig_dims_debug: None,
    };
    design.modules.push(wrapper);
    Ok(())
}

/// Extract safety properties from processes containing assert syscalls.
fn extract_properties(flat: &Flat, cls: &Class) -> Result<Vec<Property>, String> {
    let mut out = Vec::new();
    for &pi in &cls.sva_procs {
        let p = &flat.procs[pi];
        let mut guards = Vec::new();
        extract_from_stmt(&p.body, &mut guards, 0, &p.origin, &mut out)?;
    }
    Ok(out)
}

fn extract_from_stmt(
    s: &Stmt,
    guards: &mut Vec<Expr>,
    delay: usize,
    origin: &str,
    out: &mut Vec<Property>,
) -> Result<(), String> {
    match s {
        Stmt::Spanned { inner, .. } => extract_from_stmt(inner, guards, delay, origin, out),
        Stmt::SysCall { name, args, .. } if name == "sva_assert" || name == "assert" => {
            let Some(cond) = args.first() else {
                return Err("prove: malformed assert (no condition)".to_string());
            };
            out.push(Property {
                guards: guards.clone(),
                check: cond.clone(),
                delay,
                origin: origin.to_string(),
            });
            Ok(())
        }
        Stmt::SysCall { name, .. } => Err(format!(
            "prove: system task '${}' inside an assert process is not supported",
            name
        )),
        Stmt::If { cond, then, els } => {
            guards.push(cond.clone());
            extract_from_stmt(then, guards, delay, origin, out)?;
            guards.pop();
            if let Some(e) = els {
                guards.push(Expr::Un {
                    op: UnOp::Not,
                    e: Box::new(cond.clone()),
                });
                extract_from_stmt(e, guards, delay, origin, out)?;
                guards.pop();
            }
            Ok(())
        }
        Stmt::Block(ss) => {
            for s in ss {
                extract_from_stmt(s, guards, delay, origin, out)?;
            }
            Ok(())
        }
        Stmt::Wait { body, .. } => extract_from_stmt(body, guards, delay + 1, origin, out),
        Stmt::WaitUntil { .. } => Ok(()),
        // Other statements (assignments etc.) belong to the enclosing process,
        // which is still executed normally — only the asserts are extracted.
        _ => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// Encoder
// ---------------------------------------------------------------------------

struct Encoder<'a> {
    b: Builder,
    flat: &'a Flat,
    cls: Class,
    /// lits[t][sig][bit]
    lits: Vec<Vec<Vec<Lit>>>,
    comb_done: Vec<HashSet<FlatSigId>>,
}

impl<'a> Encoder<'a> {
    fn new(flat: &'a Flat, cls: Class, steps: usize) -> Result<Encoder<'a>, String> {
        let mut b = Builder::new();
        let mut lits = Vec::with_capacity(steps + 1);
        for _t in 0..=steps {
            let mut row: Vec<Vec<Lit>> = Vec::with_capacity(flat.signals.len());
            for sig in &flat.signals {
                let w = sig.width.max(1);
                let mut bits = Vec::with_capacity(w);
                for _ in 0..w {
                    bits.push(b.fresh());
                }
                row.push(bits);
            }
            lits.push(row);
        }
        Ok(Encoder {
            b,
            flat,
            cls,
            lits,
            comb_done: vec![HashSet::new(); steps + 1],
        })
    }

    fn width(&self, s: FlatSigId) -> usize {
        self.flat.signals[s as usize].width.max(1)
    }

    /// Encode all combinational writers at step t (topological order).
    fn encode_comb(&mut self, t: usize) -> Result<(), String> {
        let order = self.cls.comb_order.clone();
        for w in order {
            match w {
                Writer::Assign(ai) => {
                    let a = self.flat.assigns[ai].clone();
                    let Expr::Sig(s) = &a.lhs else {
                        return Err("prove: bad continuous assign LHS".to_string());
                    };
                    let s = *s;
                    let mut ex = Exec::comb(self, t);
                    let v = ex.eval(&a.rhs)?;
                    let bits = resize_bits(&self.b, &v.bits, self.width(s));
                    self.lits[t][s as usize] = bits;
                    self.comb_done[t].insert(s);
                }
                Writer::Proc(pi) => {
                    let body = self.flat.procs[pi].body.clone();
                    let writes = self.cls.procs[pi].1.clone();
                    let results: Vec<(FlatSigId, Vec<Lit>)> = {
                        let mut ex = Exec::comb(self, t);
                        ex.exec_stmt(&body)?;
                        let mut out = Vec::new();
                        for s in &writes {
                            let bits = match ex.env.get(s) {
                                Some(v) => v.clone(),
                                None => ex.val(*s)?,
                            };
                            out.push((*s, bits));
                        }
                        out
                    };
                    for (s, v) in results {
                        let bits = resize_bits(&self.b, &v, self.width(s));
                        self.lits[t][s as usize] = bits;
                        self.comb_done[t].insert(s);
                    }
                }
            }
        }
        Ok(())
    }

    /// Step-0 init: declaration init values, then initial blocks.
    fn encode_init(&mut self) -> Result<(), String> {
        let state = self.cls.state.clone();
        let procs = self.cls.procs.clone();
        let mut init_written: HashSet<FlatSigId> = HashSet::new();
        for (pc, writes) in &procs {
            if *pc == ProcCls::Init {
                init_written.extend(writes.iter().copied());
            }
        }
        for s in &state {
            if init_written.contains(s) {
                continue; // initial blocks override declaration init
            }
            if let Some(v) = &self.flat.signals[*s as usize].init {
                for (i, &bit) in v.bits.iter().enumerate() {
                    if i < self.width(*s) {
                        let l = self.b.const_lit(bit == V1);
                        let eq = self.b.eq_lit(self.lits[0][*s as usize][i], l);
                        self.b.clause(&[eq]);
                    }
                }
            }
        }
        for (pi, (pc, writes)) in procs.iter().enumerate() {
            if *pc != ProcCls::Init {
                continue;
            }
            let body = self.flat.procs[pi].body.clone();
            let results: Vec<(FlatSigId, Vec<Lit>)> = {
                let mut ex = Exec::at(self, 0);
                ex.exec_stmt(&body)?;
                let mut out = Vec::new();
                for s in writes {
                    if let Some(v) = ex.env.get(s) {
                        out.push((*s, v.clone()));
                    }
                }
                out
            };
            for (s, v) in results {
                let w = self.width(s);
                let bits = resize_bits(&self.b, &v, w);
                for (i, &bit) in bits.iter().enumerate().take(w) {
                    let eq = self.b.eq_lit(self.lits[0][s as usize][i], bit);
                    self.b.clause(&[eq]);
                }
            }
        }
        Ok(())
    }

    /// Transition t → t+1: run all clocked processes at step t.
    fn encode_transition(&mut self, t: usize) -> Result<(), String> {
        let procs = self.cls.procs.clone();
        let mut written: HashSet<FlatSigId> = HashSet::new();
        for (pi, (pc, writes)) in procs.iter().enumerate() {
            if *pc != ProcCls::Clocked || writes.is_empty() {
                continue; // property-only procs (e.g. `|=>`) write nothing
            }
            let body = self.flat.procs[pi].body.clone();
            let nexts: Vec<(FlatSigId, Vec<Lit>)> = {
                let mut ex = Exec::at(self, t);
                let ctrl = ex.exec_stmt(&body)?;
                if ctrl != Ctrl::Done {
                    return Err("prove: break/continue outside a loop".to_string());
                }
                // NBA results take precedence over blocking writes (they are
                // applied in the NBA region, after the active region).
                let mut out = Vec::new();
                for s in writes.iter() {
                    let next = match ex.nba_env.get(s).or_else(|| ex.env.get(s)) {
                        Some(v) => v.clone(),
                        None => ex.val(*s)?,
                    };
                    out.push((*s, next));
                }
                out
            };
            for (s, v) in nexts {
                written.insert(s);
                let w = self.width(s);
                let bits = resize_bits(&self.b, &v, w);
                for (i, &bit) in bits.iter().enumerate().take(w) {
                    let eq = self.b.eq_lit(self.lits[t + 1][s as usize][i], bit);
                    self.b.clause(&[eq]);
                }
            }
        }
        // Unwritten state holds its value.
        let state = self.cls.state.clone();
        for s in state {
            if written.contains(&s) {
                continue;
            }
            let w = self.width(s);
            for i in 0..w {
                let eq = self
                    .b
                    .eq_lit(self.lits[t + 1][s as usize][i], self.lits[t][s as usize][i]);
                self.b.clause(&[eq]);
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Symbolic execution
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ctrl {
    Done,
    Break,
    Continue,
}

/// A symbolic value: bit literals + signedness.
struct Val {
    bits: Vec<Lit>,
    signed: bool,
}

/// A compile-time constant value (used for indices / loop bounds).
struct CVal {
    bits: Vec<bool>,
    signed: bool,
}

struct Exec<'e, 'a> {
    enc: &'e mut Encoder<'a>,
    t: usize,
    env: HashMap<FlatSigId, Vec<Lit>>,
    /// Function slot space (args then locals) while executing a function body.
    locals: Option<HashMap<u32, Vec<Lit>>>,
    /// Non-blocking assign results, applied at the end of the time step.
    /// Kept separately from `env` so if/case merges can mux both.
    nba_env: HashMap<FlatSigId, Vec<Lit>>,
    /// During comb encoding: reading a not-yet-encoded comb signal is an error.
    strict_comb: bool,
}

impl<'e, 'a> Exec<'e, 'a> {
    fn at(enc: &'e mut Encoder<'a>, t: usize) -> Exec<'e, 'a> {
        Exec {
            enc,
            t,
            env: HashMap::new(),
            locals: None,
            nba_env: HashMap::new(),
            strict_comb: false,
        }
    }

    fn comb(enc: &'e mut Encoder<'a>, t: usize) -> Exec<'e, 'a> {
        let mut e = Exec::at(enc, t);
        e.strict_comb = true;
        e
    }

    fn width(&self, s: FlatSigId) -> usize {
        self.enc.width(s)
    }

    fn val(&self, s: FlatSigId) -> Result<Vec<Lit>, String> {
        if let Some(l) = &self.locals {
            if let Some(v) = l.get(&s) {
                return Ok(v.clone());
            }
        }
        if let Some(v) = self.env.get(&s) {
            return Ok(v.clone());
        }
        self.val_base(s, None)
    }

    /// Current value with an extra base layer (used for NBA part-writes, where
    /// the base is the already-recorded NBA value).
    fn val_base(
        &self,
        s: FlatSigId,
        extra: Option<&HashMap<FlatSigId, Vec<Lit>>>,
    ) -> Result<Vec<Lit>, String> {
        if let Some(e) = extra {
            if let Some(v) = e.get(&s) {
                return Ok(v.clone());
            }
        }
        if self.strict_comb && self.enc.cls.is_comb(s) && !self.enc.comb_done[self.t].contains(&s) {
            return Err(format!(
                "prove: combinational signal '{}' read before its driver (cycle?)",
                self.enc.flat.signals[s as usize].name
            ));
        }
        Ok(self.enc.lits[self.t][s as usize].clone())
    }

    // --- statements ---

    fn exec_stmt(&mut self, s: &Stmt) -> Result<Ctrl, String> {
        match s {
            Stmt::Spanned { inner, .. } => self.exec_stmt(inner),
            Stmt::Null => Ok(Ctrl::Done),
            // Functions aren't executed in BMC; treat return as done.
            Stmt::Return(_) => Ok(Ctrl::Done),
            Stmt::Block(ss) => {
                for s in ss {
                    let c = self.exec_stmt(s)?;
                    if c != Ctrl::Done {
                        return Ok(c);
                    }
                }
                Ok(Ctrl::Done)
            }
            Stmt::If { cond, then, els } => {
                let cv = self.eval(cond)?;
                let c = or_bits(&mut self.enc.b, &cv.bits);
                let saved = self.env.clone();
                let saved_nba = self.nba_env.clone();
                let ctrl_t = self.exec_stmt(then)?;
                let env_t = std::mem::replace(&mut self.env, saved.clone());
                let nba_t = std::mem::replace(&mut self.nba_env, saved_nba.clone());
                if ctrl_t != Ctrl::Done {
                    self.env = env_t;
                    self.nba_env = nba_t;
                    return Ok(ctrl_t);
                }
                let ctrl_e = match els {
                    Some(e) => self.exec_stmt(e)?,
                    None => Ctrl::Done,
                };
                let env_e = std::mem::replace(&mut self.env, saved.clone());
                let nba_e = std::mem::replace(&mut self.nba_env, saved_nba.clone());
                if ctrl_e != Ctrl::Done {
                    self.env = env_e;
                    self.nba_env = nba_e;
                    return Ok(ctrl_e);
                }
                self.merge_envs(c, &env_t, &env_e, &saved, &nba_t, &nba_e, &saved_nba)?;
                Ok(Ctrl::Done)
            }
            Stmt::Case {
                expr,
                items,
                default,
                matching,
            } => {
                // Desugar to a priority if-chain: first matching item wins.
                // BMC is 2-valued: matching-case wildcards ('-') have no SAT
                // meaning, so reject them here.
                if *matching {
                    for (conds, _) in items {
                        for c in conds {
                            if expr_has_dash(c) {
                                return Err("matching case ('-') unsupported in BMC".to_string());
                            }
                        }
                    }
                }
                let mut chain: Option<Box<Stmt>> =
                    default.as_ref().map(|d| Box::new((**d).clone()));
                for (conds, body) in items.iter().rev() {
                    let mut cond: Option<Expr> = None;
                    for c in conds {
                        let e = Expr::Bin {
                            op: BinOp::Eq,
                            l: Box::new(expr.clone()),
                            r: Box::new(c.clone()),
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
                    let cond = cond.unwrap_or_else(|| Expr::Lit(Value::from_u64(1, 0)));
                    chain = Some(Box::new(Stmt::If {
                        cond,
                        then: Box::new(body.clone()),
                        els: chain,
                    }));
                }
                match chain {
                    Some(c) => self.exec_stmt(&c),
                    None => Ok(Ctrl::Done),
                }
            }
            Stmt::Assign { lhs, rhs, nba } => {
                let v = self.eval(rhs)?;
                if *nba {
                    // NBA: RHS is evaluated now; the write lands in the NBA
                    // region (nba_env), on top of any earlier NBA to the same
                    // signal. A later NBA overrides an earlier blocking write.
                    let updates = self.plan_write_base(lhs, &v.bits, Some(&self.nba_env))?;
                    for (s, bits) in updates {
                        self.nba_env.insert(s, bits);
                    }
                } else {
                    let updates = self.plan_write(lhs, &v.bits)?;
                    for (s, bits) in updates {
                        self.env.insert(s, bits);
                    }
                }
                Ok(Ctrl::Done)
            }
            // Procedural continuous assignment/deassign are time-persistent
            // state; BMC's unrolled frames do not model them.
            Stmt::ProcContAssign { .. } => {
                Err("prove: procedural continuous assignment is not supported".to_string())
            }
            Stmt::Deassign { .. } => Err("prove: deassign is not supported in prove".to_string()),
            Stmt::For {
                init,
                cond,
                step,
                body,
            } => {
                self.exec_stmt(init)?;
                let mut iter = 0usize;
                loop {
                    let cv = self.eval(cond)?;
                    let Some(cb) = const_bits(&cv.bits, self) else {
                        return Err(
                            "prove: for-loop with non-constant condition is not supported"
                                .to_string(),
                        );
                    };
                    if !const_truthy(&cb) {
                        break;
                    }
                    let ctrl = self.exec_stmt(body)?;
                    if ctrl == Ctrl::Break {
                        break;
                    }
                    self.exec_stmt(step)?;
                    iter += 1;
                    if iter > 100_000 {
                        return Err("prove: for-loop exceeds 100000 iterations".to_string());
                    }
                }
                Ok(Ctrl::Done)
            }
            Stmt::Repeat { count, body } => {
                let Some(n) = self.const_u64(count) else {
                    return Err(
                        "prove: repeat with non-constant count is not supported".to_string()
                    );
                };
                for _ in 0..n.min(100_000) {
                    let ctrl = self.exec_stmt(body)?;
                    if ctrl == Ctrl::Break {
                        break;
                    }
                }
                Ok(Ctrl::Done)
            }
            Stmt::Break => Ok(Ctrl::Break),
            Stmt::Continue => Ok(Ctrl::Continue),
            Stmt::ExprStmt(e) => {
                let _ = self.eval(e)?;
                Ok(Ctrl::Done)
            }
            Stmt::SysCall { name, .. } if name == "sva_assert" || name == "assert" => {
                // Property check: extracted separately, no transition effect.
                Ok(Ctrl::Done)
            }
            Stmt::SysCall { name, .. } => Err(format!(
                "prove: system task '${}' is not supported in processes (only asserts)",
                name
            )),
            Stmt::Delay { .. } => Err("prove: `#delay` is not supported".to_string()),
            Stmt::AssignDelayed { .. } => {
                Err("prove: delayed signal assignment (`after`) is not supported".to_string())
            }
            Stmt::Wait { .. } => Err("prove: `@wait` is not supported in processes".to_string()),
            Stmt::WaitUntil { .. } => {
                Err("prove: `wait until` is not supported in processes".to_string())
            }
        }
    }

    /// Merge two branch states under condition `c`: written keys become
    /// c ? then-value : else-value; unwritten keys keep the base value.
    /// Merges both the blocking env and the NBA env.
    // BMC state threading: the two envs and their NBA twins; a params
    // struct would just relocate the same fields.
    #[allow(clippy::too_many_arguments)]
    fn merge_envs(
        &mut self,
        c: Lit,
        env_t: &HashMap<FlatSigId, Vec<Lit>>,
        env_e: &HashMap<FlatSigId, Vec<Lit>>,
        base: &HashMap<FlatSigId, Vec<Lit>>,
        nba_t: &HashMap<FlatSigId, Vec<Lit>>,
        nba_e: &HashMap<FlatSigId, Vec<Lit>>,
        nba_base: &HashMap<FlatSigId, Vec<Lit>>,
    ) -> Result<(), String> {
        self.merge_map(c, env_t, env_e, base, true)?;
        self.merge_map(c, nba_t, nba_e, nba_base, false)?;
        Ok(())
    }

    fn merge_map(
        &mut self,
        c: Lit,
        map_t: &HashMap<FlatSigId, Vec<Lit>>,
        map_e: &HashMap<FlatSigId, Vec<Lit>>,
        base: &HashMap<FlatSigId, Vec<Lit>>,
        into_env: bool,
    ) -> Result<(), String> {
        let mut keys: Vec<FlatSigId> = map_t.keys().chain(map_e.keys()).copied().collect();
        keys.sort_unstable();
        keys.dedup();
        for k in keys {
            let vt = map_t.get(&k).cloned();
            let ve = map_e.get(&k).cloned();
            let bv = match base.get(&k) {
                Some(v) => v.clone(),
                None => self.val(k)?,
            };
            let (a, b) = match (&vt, &ve) {
                (Some(a), Some(b)) => (a.clone(), b.clone()),
                (Some(a), None) => (a.clone(), bv.clone()),
                (None, Some(b)) => (bv.clone(), b.clone()),
                (None, None) => continue,
            };
            let bits = if a == b {
                a
            } else {
                mux_vec(&mut self.enc.b, c, &a, &b)
            };
            if into_env {
                self.env.insert(k, bits);
            } else {
                self.nba_env.insert(k, bits);
            }
        }
        Ok(())
    }

    /// Plan a write of `bits` to `lhs`: returns (sig, new value) pairs, reading
    /// current values through env / step-t lits.
    fn plan_write(&self, lhs: &Expr, bits: &[Lit]) -> Result<Vec<(FlatSigId, Vec<Lit>)>, String> {
        self.plan_write_base(lhs, bits, None)
    }

    /// Same, with an extra base layer (NBA env) consulted before `env`.
    fn plan_write_base(
        &self,
        lhs: &Expr,
        bits: &[Lit],
        extra: Option<&HashMap<FlatSigId, Vec<Lit>>>,
    ) -> Result<Vec<(FlatSigId, Vec<Lit>)>, String> {
        match lhs {
            Expr::Sig(s) => {
                let w = self.width(*s);
                Ok(vec![(*s, resize_bits(&self.enc.b, bits, w))])
            }
            Expr::Idx { e, i } => {
                let root = sig_root(e)?;
                if self.enc.flat.signals[root as usize].mem.is_some() {
                    return Err("prove: memory write not supported".to_string());
                }
                let a = self
                    .const_u64(i)
                    .ok_or("prove: non-constant bit index is not supported")?
                    as usize;
                let mut cur = self.val_base(root, extra)?;
                let w = self.width(root);
                if a < w {
                    cur[a] = resize_bits(&self.enc.b, bits, 1)[0];
                }
                Ok(vec![(root, cur)])
            }
            Expr::Sel { e, msb, lsb } => {
                let root = sig_root(e)?;
                let m = self
                    .const_u64(msb)
                    .ok_or("prove: non-constant part-select is not supported")?
                    as usize;
                let l = self
                    .const_u64(lsb)
                    .ok_or("prove: non-constant part-select is not supported")?
                    as usize;
                let mut cur = self.val_base(root, extra)?;
                let w = self.width(root);
                let msb = m.min(w.saturating_sub(1));
                let lsb = l.min(msb);
                let part = resize_bits(&self.enc.b, bits, msb - lsb + 1);
                for (k, &b) in part.iter().enumerate() {
                    cur[lsb + k] = b;
                }
                Ok(vec![(root, cur)])
            }
            Expr::Concat(parts) => {
                let mut widths = Vec::new();
                for p in parts {
                    widths.push(self.lhs_width(p)?);
                }
                let total: usize = widths.iter().sum();
                let bits = resize_bits(&self.enc.b, bits, total);
                let mut out = Vec::new();
                let mut shift = 0usize;
                for (p, w) in parts.iter().zip(widths.iter()) {
                    let sub: Vec<Lit> = bits[shift..shift + *w].to_vec();
                    out.extend(self.plan_write_base(p, &sub, extra)?);
                    shift += *w;
                }
                Ok(out)
            }
            _ => Err("prove: unsupported assignment LHS".to_string()),
        }
    }

    fn lhs_width(&self, lhs: &Expr) -> Result<usize, String> {
        match lhs {
            Expr::Sig(s) => Ok(self.width(*s)),
            Expr::Idx { .. } => Ok(1),
            Expr::Sel { msb, lsb, .. } => {
                let m = self.const_u64(msb).ok_or("prove: non-constant select")? as usize;
                let l = self.const_u64(lsb).ok_or("prove: non-constant select")? as usize;
                Ok(m.saturating_sub(l) + 1)
            }
            Expr::Concat(parts) => {
                let mut t = 0;
                for p in parts {
                    t += self.lhs_width(p)?;
                }
                Ok(t)
            }
            _ => Err("prove: unsupported LHS in concat".to_string()),
        }
    }

    // --- expressions ---

    fn eval(&mut self, e: &Expr) -> Result<Val, String> {
        match e {
            Expr::SigAttr { .. } => Err(
                "prove: runtime sig'length/'high on unconstrained ports unsupported".to_string(),
            ),
            Expr::Real(_) => {
                Err("prove: real-typed expressions are unsupported in BMC".to_string())
            }
            Expr::Lit(v) => Ok(Val {
                // 2-valued: X/Z/… literal bits encode as 0.
                bits: v
                    .bits
                    .iter()
                    .map(|&b| self.enc.b.const_lit(b == V1))
                    .collect(),
                signed: false,
            }),
            Expr::Sig(s) => {
                let signed = self.enc.flat.signals[*s as usize].signed;
                Ok(Val {
                    bits: self.val(*s)?,
                    signed,
                })
            }
            Expr::Bin { op, l, r } => {
                let lv = self.eval(l)?;
                let rv = self.eval(r)?;
                let w = lv.bits.len().max(rv.bits.len()).max(1);
                let signed = lv.signed || rv.signed;
                let b = &mut self.enc.b;
                let l2 = extend(b, &lv.bits, w, signed);
                let r2 = extend(b, &rv.bits, w, signed);
                // Matching operators have don't-care semantics that are not
                // 2-valued; reject rather than give a wrong answer.
                if matches!(
                    op,
                    BinOp::EqM | BinOp::NeM | BinOp::LtM | BinOp::LeM | BinOp::GtM | BinOp::GeM
                ) {
                    return Err("prove: matching operators (?=, ?/=, ...) unsupported".to_string());
                }
                let bits = match op {
                    BinOp::Add => add_bits(b, &l2, &r2, b.false_lit(), w).0,
                    BinOp::Sub => sub_bits(b, &l2, &r2, w),
                    BinOp::Mul => mul_bits(b, &l2, &r2, w),
                    BinOp::Div => divmod_bits(b, &l2, &r2, w).0,
                    BinOp::Mod => divmod_bits(b, &l2, &r2, w).1,
                    BinOp::Shl => {
                        let m = extend(b, &rv.bits, 6, false);
                        shift_bits(b, &l2, &m, w, ShiftDir::Left, None)
                    }
                    BinOp::Shr => {
                        let m = extend(b, &rv.bits, 6, false);
                        shift_bits(b, &l2, &m, w, ShiftDir::Right, None)
                    }
                    BinOp::Sar => {
                        let m = extend(b, &rv.bits, 6, false);
                        let sign = if w > 0 { Some(l2[w - 1]) } else { None };
                        shift_bits(b, &l2, &m, w, ShiftDir::Right, sign)
                    }
                    BinOp::Rol | BinOp::Ror => {
                        return Err("prove: rotate operators (rol/ror) unsupported".to_string());
                    }
                    BinOp::Pow => {
                        return Err("prove: '**' operator unsupported".to_string());
                    }
                    BinOp::Lt => vec![lt_bits(b, &l2, &r2, w, signed)],
                    BinOp::Le => {
                        let lt = lt_bits(b, &l2, &r2, w, signed);
                        let eq = eq_bits(b, &l2, &r2, w);
                        vec![b.or_lit(lt, eq)]
                    }
                    BinOp::Gt => vec![lt_bits(b, &r2, &l2, w, signed)],
                    BinOp::Ge => vec![ge_bits(b, &l2, &r2, w, signed)],
                    BinOp::Eq | BinOp::EqStrict => vec![eq_bits(b, &l2, &r2, w)],
                    BinOp::Ne | BinOp::NeStrict => vec![neg(eq_bits(b, &l2, &r2, w))],
                    BinOp::BAnd => and_vec(b, &l2, &r2),
                    BinOp::BOr => or_vec(b, &l2, &r2),
                    BinOp::BXor => xor_vec(b, &l2, &r2),
                    BinOp::BNand => not_bits(&and_vec(b, &l2, &r2)),
                    BinOp::BNor => not_bits(&or_vec(b, &l2, &r2)),
                    BinOp::BXnor => not_bits(&xor_vec(b, &l2, &r2)),
                    BinOp::LogAnd => {
                        let ta = or_bits(b, &l2);
                        let tb = or_bits(b, &r2);
                        vec![b.and_lit(ta, tb)]
                    }
                    BinOp::LogOr => {
                        let ta = or_bits(b, &l2);
                        let tb = or_bits(b, &r2);
                        vec![b.or_lit(ta, tb)]
                    }
                    // Rejected by the guard above.
                    BinOp::EqM | BinOp::NeM | BinOp::LtM | BinOp::LeM | BinOp::GtM | BinOp::GeM => {
                        vec![b.false_lit()]
                    }
                };
                Ok(Val { bits, signed })
            }
            Expr::Un { op, e } => {
                let v = self.eval(e)?;
                let b = &mut self.enc.b;
                let w = v.bits.len().max(1);
                let bits = match op {
                    UnOp::Not => vec![neg(or_bits(b, &v.bits))],
                    UnOp::BNot => not_bits(&v.bits),
                    UnOp::Neg => sub_bits(b, &vec![b.false_lit(); w], &v.bits, w),
                    UnOp::Pos => v.bits.clone(),
                    // `abs` in BMC: approximated as identity (sign handling is
                    // not modeled symbolically).
                    UnOp::Abs => v.bits.clone(),
                    UnOp::RedAnd => vec![and_bits(b, &v.bits)],
                    UnOp::RedOr => vec![or_bits(b, &v.bits)],
                    UnOp::RedXor => {
                        let mut acc = b.false_lit();
                        for &x in &v.bits {
                            acc = b.xor_lit(acc, x);
                        }
                        vec![acc]
                    }
                    UnOp::RedNand => vec![neg(and_bits(b, &v.bits))],
                    UnOp::RedNor => vec![neg(or_bits(b, &v.bits))],
                    UnOp::RedXnor => {
                        let mut acc = b.false_lit();
                        for &x in &v.bits {
                            acc = b.xor_lit(acc, x);
                        }
                        vec![neg(acc)]
                    }
                };
                Ok(Val {
                    bits,
                    signed: false,
                })
            }
            Expr::Cond { c, t, e } => {
                let cv = self.eval(c)?;
                let tv = self.eval(t)?;
                let ev = self.eval(e)?;
                let w = tv.bits.len().max(ev.bits.len()).max(1);
                let b = &mut self.enc.b;
                let t2 = extend(b, &tv.bits, w, tv.signed);
                let e2 = extend(b, &ev.bits, w, ev.signed);
                let cond = or_bits(b, &cv.bits);
                Ok(Val {
                    bits: mux_vec(b, cond, &t2, &e2),
                    signed: tv.signed || ev.signed,
                })
            }
            Expr::Call { name, args, shapes } => self.eval_call(name, args, shapes),
            Expr::Concat(parts) => {
                let mut vals = Vec::new();
                for p in parts {
                    vals.push(self.eval(p)?);
                }
                // parts[0] is most significant: its bits go to the high end.
                let mut bits = Vec::new();
                for v in vals.iter().rev() {
                    bits.extend_from_slice(&v.bits);
                }
                Ok(Val {
                    bits,
                    signed: false,
                })
            }
            Expr::Stream { .. } => {
                Err("prove: SV streaming operator is unsupported in BMC".to_string())
            }
            Expr::Replicate { count, e } => {
                let n = self
                    .const_u64(count)
                    .ok_or("prove: non-constant replication count is not supported")?
                    as usize;
                let v = self.eval(e)?;
                let mut bits = Vec::new();
                for _ in 0..n.min(4096) {
                    bits.extend_from_slice(&v.bits);
                }
                Ok(Val {
                    bits,
                    signed: v.signed,
                })
            }
            Expr::Sel { e, msb, lsb } => {
                let v = self.eval(e)?;
                let m = self
                    .const_u64(msb)
                    .ok_or("prove: non-constant part-select is not supported")?
                    as usize;
                let l = self
                    .const_u64(lsb)
                    .ok_or("prove: non-constant part-select is not supported")?
                    as usize;
                let len = v.bits.len();
                let msb = m.min(len.saturating_sub(1));
                let lsb = l.min(msb);
                Ok(Val {
                    bits: v.bits[lsb..=msb].to_vec(),
                    signed: v.signed,
                })
            }
            Expr::Idx { e, i } => {
                let a = self
                    .const_u64(i)
                    .ok_or("prove: non-constant bit index is not supported")?
                    as usize;
                if let Expr::Sig(s) = &**e {
                    if self.enc.flat.signals[*s as usize].mem.is_some() {
                        return Err("prove: memory read not supported".to_string());
                    }
                }
                let v = self.eval(e)?;
                let bit = if a < v.bits.len() {
                    v.bits[a]
                } else {
                    self.enc.b.false_lit()
                };
                Ok(Val {
                    bits: vec![bit],
                    signed: false,
                })
            }
            // Every BMC step is a clock edge: rising_edge is true, falling false.
            Expr::Rising(_) => Ok(Val {
                bits: vec![self.enc.b.true_lit()],
                signed: false,
            }),
            Expr::Falling(_) => Ok(Val {
                bits: vec![self.enc.b.false_lit()],
                signed: false,
            }),
            Expr::Prev(_) => Err("prove: 'last_value (Prev) unsupported".to_string()),
            Expr::ToInt(e) => {
                let v = self.eval(e)?;
                let b = &self.enc.b;
                Ok(Val {
                    bits: extend(b, &v.bits, 32, false),
                    signed: false,
                })
            }
            Expr::ToString { .. } => Err("prove: string conversion is not supported".to_string()),
            Expr::Resize { e, width, signed } => {
                let v = self.eval(e)?;
                let b = &self.enc.b;
                Ok(Val {
                    bits: extend(b, &v.bits, *width, *signed),
                    signed: *signed,
                })
            }
            Expr::Signed(e) => {
                let mut v = self.eval(e)?;
                v.signed = true;
                Ok(v)
            }
        }
    }

    fn eval_call(
        &mut self,
        name: &str,
        args: &[Expr],
        shapes: &[ShapeBind],
    ) -> Result<Val, String> {
        match name {
            "clog2" => {
                let v = self
                    .const_u64(&args[0])
                    .ok_or("prove: $clog2 with non-constant argument is not supported")?;
                let n = (64 - v.leading_zeros()).max(1) as u64;
                Ok(Val {
                    bits: (0..32)
                        .map(|i| self.enc.b.const_lit((n >> i) & 1 == 1))
                        .collect(),
                    signed: false,
                })
            }
            "unsigned" => {
                let mut v = self.eval(&args[0])?;
                v.signed = false;
                Ok(v)
            }
            "signed" => {
                let mut v = self.eval(&args[0])?;
                v.signed = true;
                Ok(v)
            }
            _ => {
                // User function: inline by executing its body in slot space.
                let fi = self
                    .enc
                    .flat
                    .functions
                    .iter()
                    .position(|f| f.name == name)
                    .ok_or_else(|| format!("prove: unknown function '{}'", name))?;
                let f = self.enc.flat.functions[fi].clone();
                let mut argv = Vec::new();
                for a in args {
                    argv.push(self.eval(a)?.bits);
                }
                let mut locals: HashMap<u32, Vec<Lit>> = HashMap::new();
                for (i, av) in argv.iter().enumerate() {
                    let w = f.slots.get(i).copied().unwrap_or(1).max(1);
                    let bits = resize_bits(&self.enc.b, av, w);
                    locals.insert(i as u32, bits);
                }
                // Hidden shape slots: the bound lo/asc evaluate in the
                // caller's context; an unbound formal takes its fallback
                // convention (see `Sim::fill_hidden`).
                for h in &f.hidden {
                    let (lo, asc) = match shapes.iter().find(|b| b.formal == h.formal) {
                        Some(b) => (self.eval(&b.lo)?.bits, self.eval(&b.asc)?.bits),
                        None => {
                            let fb = |v: u64| {
                                (0..32)
                                    .map(|i| self.enc.b.const_lit((v >> i) & 1 == 1))
                                    .collect::<Vec<_>>()
                            };
                            (fb(h.fb_lo.max(0) as u64), fb(h.fb_asc as u64))
                        }
                    };
                    locals.insert(h.lo, resize_bits(&self.enc.b, &lo, 32));
                    locals.insert(h.asc, resize_bits(&self.enc.b, &asc, 32));
                }
                let saved_locals = self.locals.replace(locals);
                let saved_env = std::mem::take(&mut self.env);
                let saved_nba = std::mem::take(&mut self.nba_env);
                let r = self.exec_stmt(&f.body);
                let mut locals = self.locals.take().unwrap_or_default();
                self.locals = saved_locals;
                self.env = saved_env;
                self.nba_env = saved_nba;
                r?;
                let ret = locals
                    .remove(&0)
                    .unwrap_or_else(|| vec![self.enc.b.false_lit()]);
                Ok(Val {
                    bits: resize_bits(&self.enc.b, &ret, f.ret_width.max(1)),
                    signed: false,
                })
            }
        }
    }

    // --- constant folding (no gates) for indices / loop bounds ---

    fn const_u64(&self, e: &Expr) -> Option<u64> {
        let v = self.const_val(e)?;
        let mut acc: u64 = 0;
        for (i, &b) in v.bits.iter().enumerate() {
            if i < 64 && b {
                acc |= 1 << i;
            }
        }
        Some(acc)
    }

    fn const_val(&self, e: &Expr) -> Option<CVal> {
        match e {
            Expr::Lit(v) => Some(CVal {
                bits: v.bits.iter().map(|&b| b == V1).collect(),
                signed: false,
            }),
            Expr::Sig(s) => {
                let lits = if let Some(l) = &self.locals {
                    l.get(s).cloned()
                } else {
                    self.env.get(s).cloned()
                }?;
                let bits: Vec<bool> = lits
                    .iter()
                    .map(|&l| {
                        if l == self.enc.b.true_lit() {
                            Some(true)
                        } else if l == self.enc.b.false_lit() {
                            Some(false)
                        } else {
                            None
                        }
                    })
                    .collect::<Option<Vec<bool>>>()?;
                Some(CVal {
                    bits,
                    signed: self.enc.flat.signals[*s as usize].signed,
                })
            }
            Expr::Bin { op, l, r } => {
                let lv = self.const_val(l)?;
                let rv = self.const_val(r)?;
                let w = lv.bits.len().max(rv.bits.len()).max(1);
                let signed = lv.signed || rv.signed;
                let l2 = const_extend(&lv.bits, w, signed);
                let r2 = const_extend(&rv.bits, w, signed);
                let bits = const_binop(*op, &l2, &r2, w, signed)?;
                Some(CVal { bits, signed })
            }
            Expr::Un { op, e } => {
                let v = self.const_val(e)?;
                Some(CVal {
                    bits: const_unop(*op, &v.bits)?,
                    signed: false,
                })
            }
            Expr::Cond { c, t, e } => {
                let cv = self.const_val(c)?;
                if const_truthy(&cv.bits) {
                    self.const_val(t)
                } else {
                    self.const_val(e)
                }
            }
            Expr::Resize { e, width, signed } => {
                let v = self.const_val(e)?;
                Some(CVal {
                    bits: const_extend(&v.bits, *width, *signed),
                    signed: *signed,
                })
            }
            Expr::Signed(e) => {
                let mut v = self.const_val(e)?;
                v.signed = true;
                Some(v)
            }
            _ => None,
        }
    }
}

/// If all bits are constant literals, return them as bools.
fn const_bits(bits: &[Lit], ex: &Exec) -> Option<Vec<bool>> {
    bits.iter()
        .map(|&l| {
            if l == ex.enc.b.true_lit() {
                Some(true)
            } else if l == ex.enc.b.false_lit() {
                Some(false)
            } else {
                None
            }
        })
        .collect()
}

fn const_truthy(bits: &[bool]) -> bool {
    bits.iter().any(|&b| b)
}

fn const_extend(bits: &[bool], w: usize, signed: bool) -> Vec<bool> {
    let mut out = bits.to_vec();
    if out.len() > w {
        out.truncate(w);
    } else {
        let fill = signed && out.last().copied().unwrap_or(false);
        while out.len() < w {
            out.push(fill);
        }
    }
    out
}

fn mask(w: usize) -> u64 {
    if w >= 64 {
        u64::MAX
    } else {
        (1u64 << w) - 1
    }
}

fn const_binop(op: BinOp, a: &[bool], b: &[bool], w: usize, signed: bool) -> Option<Vec<bool>> {
    let to_u = |v: &[bool]| -> u64 {
        let mut x = 0u64;
        for (i, &bit) in v.iter().enumerate() {
            if i < 64 && bit {
                x |= 1 << i;
            }
        }
        x
    };
    let to_i = |v: &[bool]| -> i128 {
        let mut x: i128 = 0;
        for (i, &bit) in v.iter().enumerate() {
            if bit {
                x |= 1 << i;
            }
        }
        if signed && w < 128 && v.get(w - 1).copied().unwrap_or(false) {
            x |= -1i128 << w;
        }
        x
    };
    let bits_of = |v: u64| -> Vec<bool> { (0..w).map(|i| (v >> i) & 1 == 1).collect() };
    let r = match op {
        BinOp::Add => to_u(a).wrapping_add(to_u(b)),
        BinOp::Sub => to_u(a).wrapping_sub(to_u(b)),
        BinOp::Mul => to_u(a).wrapping_mul(to_u(b)),
        BinOp::Div => to_u(a).checked_div(to_u(b)).unwrap_or(0),
        BinOp::Mod => {
            let (x, y) = (to_u(a), to_u(b));
            if y == 0 {
                0
            } else {
                x % y
            }
        }
        BinOp::Shl => to_u(a).wrapping_shl(to_u(b) as u32),
        BinOp::Shr => to_u(a).wrapping_shr(to_u(b) as u32),
        BinOp::Sar => {
            let sh = (to_u(b) as u32) % 64;
            if signed {
                ((to_i(a) >> sh) as u64) & mask(w)
            } else {
                to_u(a) >> sh
            }
        }
        _ => {
            return Some(match op {
                BinOp::Lt => vec![if signed {
                    to_i(a) < to_i(b)
                } else {
                    to_u(a) < to_u(b)
                }],
                BinOp::Le => vec![if signed {
                    to_i(a) <= to_i(b)
                } else {
                    to_u(a) <= to_u(b)
                }],
                BinOp::Gt => vec![if signed {
                    to_i(a) > to_i(b)
                } else {
                    to_u(a) > to_u(b)
                }],
                BinOp::Ge => vec![if signed {
                    to_i(a) >= to_i(b)
                } else {
                    to_u(a) >= to_u(b)
                }],
                BinOp::Eq | BinOp::EqStrict => vec![a == b],
                BinOp::Ne | BinOp::NeStrict => vec![a != b],
                BinOp::BAnd => (0..w).map(|i| a[i] && b[i]).collect(),
                BinOp::BOr => (0..w).map(|i| a[i] || b[i]).collect(),
                BinOp::BXor => (0..w).map(|i| a[i] != b[i]).collect(),
                BinOp::BNand => (0..w).map(|i| !(a[i] && b[i])).collect(),
                BinOp::BNor => (0..w).map(|i| !(a[i] || b[i])).collect(),
                BinOp::BXnor => (0..w).map(|i| a[i] == b[i]).collect(),
                BinOp::LogAnd => vec![to_u(a) != 0 && to_u(b) != 0],
                BinOp::LogOr => vec![to_u(a) != 0 || to_u(b) != 0],
                _ => return None,
            });
        }
    };
    Some(bits_of(r))
}

fn const_unop(op: UnOp, a: &[bool]) -> Option<Vec<bool>> {
    let w = a.len().max(1);
    Some(match op {
        UnOp::Not => vec![!a.iter().any(|&b| b)],
        UnOp::BNot => a.iter().map(|&b| !b).collect(),
        UnOp::Neg => {
            let mut x = 0u64;
            for (i, &b) in a.iter().enumerate() {
                if i < 64 && b {
                    x |= 1 << i;
                }
            }
            (0..w).map(|i| (x.wrapping_neg() >> i) & 1 == 1).collect()
        }
        UnOp::Pos => a.to_vec(),
        UnOp::Abs => {
            // two's-complement absolute value
            let sign = a.last().copied().unwrap_or(false);
            if sign {
                let mut x = 0u64;
                for (i, &b) in a.iter().enumerate() {
                    if i < 64 && b {
                        x |= 1 << i;
                    }
                }
                (0..a.len())
                    .map(|i| (x.wrapping_neg() >> i) & 1 == 1)
                    .collect()
            } else {
                a.to_vec()
            }
        }
        UnOp::RedAnd => vec![a.iter().all(|&b| b)],
        UnOp::RedOr => vec![a.iter().any(|&b| b)],
        UnOp::RedXor => vec![a.iter().fold(false, |acc, &b| acc != b)],
        UnOp::RedNand => vec![!a.iter().all(|&b| b)],
        UnOp::RedNor => vec![!a.iter().any(|&b| b)],
        UnOp::RedXnor => vec![!a.iter().fold(false, |acc, &b| acc != b)],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use hdl_ir::Design;
    use hdl_syntax::diagnostic::Severity;
    use hdl_syntax::source::SourceDb;

    use hdl_ir::elab::elaborate;

    /// Parse + lower a Verilog source string into a Design.
    fn lower_top(src: &str, top: &str) -> Design {
        let mut db = SourceDb::new();
        let file = db.add_file("test.v", src).expect("add_file");
        let parsed = hdl_syntax::verilog::parser::parse_file(file, db.file(file));
        assert!(
            parsed.diags.iter().all(|d| d.severity != Severity::Error),
            "parse errors: {:?}",
            parsed.diags
        );
        let mut diags = Vec::new();
        let modules = hdl_ir::verilog_lower::lower_file(file, &parsed.root, &db, &mut diags);
        assert!(
            diags.iter().all(|d| d.severity != Severity::Error),
            "lower errors: {:?}",
            diags
        );
        Design {
            modules,
            top: Some(top.to_string()),
            packages: Default::default(),
            pkg_funcs: Default::default(),
            configurations: Default::default(),
        }
    }

    /// Parse + lower + wrap + elaborate + prove a Verilog source string.
    fn prove_src(src: &str, top: &str, steps: usize) -> Result<ProveResult, String> {
        let mut design = lower_top(src, top);
        wrap_top(&mut design, top)?;
        let flat = elaborate(&design, "__hdlsim_prove_top")
            .map_err(|e| format!("elaboration error: {}", e))?;
        prove(&flat, steps)
    }

    #[test]
    fn prove_safe_register() {
        // q <= d; an 8-bit q is always < 256 → safe.
        let src = "module dut(input wire clk, input wire [7:0] d, output reg [7:0] q);\n\
                   always @(posedge clk) q <= d;\n\
                   assert property (@(posedge clk) q < 256);\n\
                   endmodule\n";
        let r = prove_src(src, "dut", 4).expect("prove");
        assert!(matches!(r.status, ProveStatus::Safe), "{:?}", r.status);
    }

    #[test]
    fn prove_finds_violation() {
        // q <= ~d, but the property claims q == d one cycle later → fails.
        let src = "module dut(input wire clk, input wire [7:0] d, output reg [7:0] q);\n\
                   always @(posedge clk) q <= ~d;\n\
                   assert property (@(posedge clk) d |=> q == d);\n\
                   endmodule\n";
        let r = prove_src(src, "dut", 4).expect("prove");
        match r.status {
            ProveStatus::Violation(pi, t) => {
                assert_eq!(pi, 0);
                assert!(t <= 3, "violation step {}", t);
                assert_eq!(r.trace.len(), 5);
                // In the counterexample, q at step 1 == ~d at step 0.
                let d0 = r.trace[0].iter().find(|(n, _)| n == "d").unwrap().1;
                let q1 = r.trace[1].iter().find(|(n, _)| n == "q").unwrap().1;
                assert_eq!(q1, (!d0) & 0xff, "cex: d0={} q1={}", d0, q1);
            }
            other => panic!("expected violation, got {:?}", other),
        }
    }

    #[test]
    fn prove_overlap_property_fails() {
        // b <= a; `a |-> b` must fail (b holds the old value at the same edge).
        let src = "module dut(input wire clk, input wire a, output reg b);\n\
                   always @(posedge clk) b <= a;\n\
                   assert property (@(posedge clk) a |-> b);\n\
                   endmodule\n";
        let r = prove_src(src, "dut", 3).expect("prove");
        assert!(
            matches!(r.status, ProveStatus::Violation(0, _)),
            "{:?}",
            r.status
        );
    }

    #[test]
    fn prove_counter_reaches_fifteen() {
        // 4-bit counter from 0: `cnt != 15` is violated exactly at step 15.
        let src = "module dut(input wire clk, output reg [3:0] cnt);\n\
                   initial cnt = 0;\n\
                   always @(posedge clk) cnt <= cnt + 1;\n\
                   assert property (@(posedge clk) cnt != 4'd15);\n\
                   endmodule\n";
        let r = prove_src(src, "dut", 20).expect("prove");
        match r.status {
            ProveStatus::Violation(0, t) => assert_eq!(t, 15, "cnt reaches 15 at step 15"),
            other => panic!("expected violation, got {:?}", other),
        }
    }

    #[test]
    fn prove_counter_bound_holds() {
        // Same counter: `cnt < 15` holds for steps 0..=14.
        let src = "module dut(input wire clk, output reg [3:0] cnt);\n\
                   initial cnt = 0;\n\
                   always @(posedge clk) cnt <= cnt + 1;\n\
                   assert property (@(posedge clk) cnt < 15);\n\
                   endmodule\n";
        let r = prove_src(src, "dut", 14).expect("prove");
        assert!(matches!(r.status, ProveStatus::Safe), "{:?}", r.status);
    }

    #[test]
    fn prove_comb_logic_safe() {
        // y = a & b; the property restates the assign → always safe.
        let src = "module dut(input wire clk, input wire [3:0] a, input wire [3:0] b, output wire [3:0] y);\n\
                   assign y = a & b;\n\
                   assert property (@(posedge clk) y == (a & b));\n\
                   endmodule\n";
        let r = prove_src(src, "dut", 2).expect("prove");
        assert!(matches!(r.status, ProveStatus::Safe), "{:?}", r.status);
    }

    #[test]
    fn prove_comb_logic_violation() {
        // y = a & b but the property claims y == (a | b) → violated when
        // exactly one of a, b is nonzero.
        let src = "module dut(input wire clk, input wire [3:0] a, input wire [3:0] b, output wire [3:0] y);\n\
                   assign y = a & b;\n\
                   assert property (@(posedge clk) y == (a | b));\n\
                   endmodule\n";
        let r = prove_src(src, "dut", 2).expect("prove");
        assert!(
            matches!(r.status, ProveStatus::Violation(0, 0)),
            "{:?}",
            r.status
        );
    }

    #[test]
    fn prove_rejects_memories() {
        let src = "module dut(input wire clk, input wire [3:0] d, output reg [7:0] q);\n\
                   reg [7:0] mem [0:15];\n\
                   always @(posedge clk) begin mem[0] <= d; q <= mem[0]; end\n\
                   assert property (@(posedge clk) q == 0);\n\
                   endmodule\n";
        assert!(prove_src(src, "dut", 2).unwrap_err().contains("memories"));
    }

    #[test]
    fn prove_rejects_delay() {
        let src = "module dut(input wire clk, output reg q);\n\
                   initial begin q = 0; #10 q = 1; end\n\
                   assert property (@(posedge clk) q == 0);\n\
                   endmodule\n";
        assert!(prove_src(src, "dut", 2).unwrap_err().contains("#delay"));
    }

    #[test]
    fn prove_arithmetic_and_case() {
        // 2-bit FSM-ish: state machine with case; assert state never 3.
        let src = "module dut(input wire clk, input wire rst, output reg [1:0] st);\n\
                   initial st = 0;\n\
                   always @(posedge clk) begin\n\
                     if (rst) st <= 0;\n\
                     else case (st)\n\
                       0: st <= 1;\n\
                       1: st <= 2;\n\
                       2: st <= 0;\n\
                       default: st <= 3;\n\
                     endcase\n\
                   end\n\
                   assert property (@(posedge clk) st != 2'd3);\n\
                   endmodule\n";
        let r = prove_src(src, "dut", 10).expect("prove");
        assert!(matches!(r.status, ProveStatus::Safe), "{:?}", r.status);
    }

    #[test]
    fn prove_async_reset() {
        // Async reset: st resets to 0 whenever rst=1; property st < 4 always
        // holds for a 2-bit reg → safe; also checks rst as a free input.
        let src = "module dut(input wire clk, input wire rst, output reg [1:0] st);\n\
                   always @(posedge clk or posedge rst) begin\n\
                     if (rst) st <= 0; else st <= st + 1;\n\
                   end\n\
                   assert property (@(posedge clk) st < 4);\n\
                   endmodule\n";
        let r = prove_src(src, "dut", 6).expect("prove");
        assert!(matches!(r.status, ProveStatus::Safe), "{:?}", r.status);
    }
}
