//! Elaboration: flatten the instance hierarchy into one flat signal space that
//! the simulation kernel executes directly.
//!
//! Strategy: recursive instantiation. Each module instance gets a scope; its
//! signals become flat signals with hierarchical names (`tb.dut.q`) used by the
//! wave display. Port connections that are plain signals become *aliases* (no
//! extra signal); anything else becomes an implicit wire plus a continuous
//! assign in the appropriate direction.
//!
//! Invariants:
//! - After elaboration every `Expr::Sig`/`Stmt` signal id indexes `Flat::signals`.
//! - `Flat::scopes` forms a tree rooted at `top_scope`.
//! - Elaboration errors are `Err(String)` with the instance path — no panics.

use crate::ir::*;
use crate::value::Value;
use std::collections::HashMap;
use std::sync::Arc;

use serde::Serialize;

/// Field-level serializer for `FlatFunc::body` (`Arc<Stmt>`): serde needs the
/// `rc` feature for `Arc<T>`, which this crate does not enable — serialize
/// through the inner `Stmt` instead.
pub mod serde_arc {
    use super::Stmt;
    use serde::{Serialize, Serializer};
    use std::sync::Arc;
    pub fn stmt<S: Serializer>(b: &Arc<Stmt>, s: S) -> Result<S::Ok, S::Error> {
        Stmt::serialize(&**b, s)
    }
}

pub type FlatSigId = u32;
pub type ScopeId = u32;

#[derive(Clone, Debug, Serialize)]
pub struct Scope {
    pub name: String,
    pub parent: Option<ScopeId>,
}

#[derive(Clone, Debug, Serialize)]
pub struct FlatSig {
    /// Full hierarchical name, e.g. `tb.dut.q`.
    pub name: String,
    pub width: usize,
    pub kind: SigKind,
    pub init: Option<Value>,
    /// `Some(depth)` for memories.
    pub mem: Option<usize>,
    pub scope: ScopeId,
    /// Signed arithmetic/comparison (VHDL `signed`, Verilog `signed`).
    pub signed: bool,
    /// Inclusive bit ranges of real-typed record fields: the simulator
    /// decodes those slices as IEEE-754 (see `ModuleIr::real_ranges`).
    pub real_ranges: Vec<(u32, u32)>,
}

#[derive(Clone, Debug, Serialize)]
pub struct FlatProc {
    pub kind: ProcKind,
    pub sens: Sens,
    pub body: Stmt,
    pub origin: String,
    /// Run once at t=0 before waiting on `sens` (VHDL processes).
    pub initial: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct FlatAssign {
    pub lhs: Expr,
    pub rhs: Expr,
}

/// A function body with its local slot space (`args` then `locals`).
#[derive(Clone, Debug, Serialize)]
pub struct FlatFunc {
    pub name: String,
    pub ret_width: usize,
    /// Number of formal arguments — resolves overloads (same name,
    /// different parameter count) when the simulator looks a call up.
    pub arity: usize,
    /// Width of each slot; slot ids in the body index this vector.
    pub slots: Vec<usize>,
    /// Lowercase declared type name per formal (`""` = unknown). Same-width
    /// unconstrained overloads (`signed` vs `unsigned` vs
    /// `std_ulogic_vector`, all w1) are distinguished by this at dispatch —
    /// widths alone cannot separate them, and a wrong pick lets the
    /// delegating overload bodies recurse to the depth guard.
    pub formal_types: Vec<String>,
    /// Recorded interface defaults, one per formal (`None` where the formal
    /// has none). A call may omit trailing actuals whose entries are `Some`;
    /// empty means no formal carries a default.
    pub defaults: Vec<Option<Value>>,
    /// Shared handle: the kernel can execute a body while mutably borrowing
    /// the simulator (procedure calls re-enter), and cloning stays cheap.
    #[serde(serialize_with = "serde_arc::stmt")]
    pub body: Arc<Stmt>,
}

/// The elaborated design for one top: flat signals, processes, continuous
/// assigns, and package/function bodies — what the kernel executes.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Flat {
    pub signals: Vec<FlatSig>,
    pub procs: Vec<FlatProc>,
    pub assigns: Vec<FlatAssign>,
    pub functions: Vec<FlatFunc>,
    pub scopes: Vec<Scope>,
    pub top_scope: ScopeId,
}

/// Elaborate `design` with `top` as the root. Params use defaults (overrides are
/// applied per-instance during instantiation by the lowerer).
/// Elaboration errors. `UnknownUnit` is retryable: a [`UnitResolver`] may load
/// the missing unit and elaboration restarts.
#[derive(Debug, thiserror::Error)]
pub enum ElabError {
    /// A design unit referenced by an instantiation is not compiled yet.
    #[error("{where_}: unknown design unit '{name}'")]
    UnknownUnit { name: String, where_: String },
    /// Any other elaboration failure.
    #[error("{0}")]
    Message(String),
}

/// Resolution seam: when elaboration meets a design unit that is not in the
/// `Design` yet, it asks the resolver to compile/load it into `design`
/// (e.g. by searching libraries). Returns `Ok(true)` if the unit is now
/// available and elaboration should retry.
pub trait UnitResolver {
    fn resolve(&mut self, design: &mut Design, name: &str) -> Result<bool, String>;
    /// Produce (or reuse) a variant of `unit` (a module name, possibly
    /// `entity/arch`) re-lowered with the constant `params` overrides from an
    /// instantiation's `generic map` / `#(...)`; returns the variant module
    /// name, or `None` when unsupported (elaboration then falls back to the
    /// default-generics module). `bound` is the design-modules index of the
    /// module the instantiation resolved to — its source file disambiguates
    /// same-named entities from different files.
    fn resolve_variant(
        &mut self,
        _design: &mut Design,
        _unit: &str,
        _params: &HashMap<String, Value>,
        _bound: usize,
    ) -> Result<Option<String>, String> {
        Ok(None)
    }
}

/// Resolver that never loads anything (all units must be compiled up front).
struct NoResolver;

impl UnitResolver for NoResolver {
    fn resolve(&mut self, _design: &mut Design, _name: &str) -> Result<bool, String> {
        Ok(false)
    }
}

pub fn elaborate(design: &Design, top: &str) -> Result<Flat, ElabError> {
    let mut d = design.clone();
    elaborate_with(&mut d, top, &mut NoResolver)
}

/// Elaborate `top`, loading missing design units through `resolver` on demand.
pub fn elaborate_with(
    design: &mut Design,
    top: &str,
    resolver: &mut dyn UnitResolver,
) -> Result<Flat, ElabError> {
    elaborate_with_opts(design, top, resolver, true)
}

/// Like [`elaborate_with`], but `register_pkg_funcs = false` skips copying all
/// package function bodies into the result — used by `check`, which elaborates
/// every module as a candidate top only to surface unresolved units, so the
/// per-top O(all package bodies) clone is pure waste there.
pub fn elaborate_with_opts(
    design: &mut Design,
    top: &str,
    resolver: &mut dyn UnitResolver,
    register_pkg_funcs: bool,
) -> Result<Flat, ElabError> {
    // The top itself may live in a library: resolve it before elaborating.
    if !design.modules.iter().any(|m| m.name == top) {
        resolver.resolve(design, top).map_err(ElabError::Message)?;
    }
    let mut tried: Vec<String> = Vec::new();
    loop {
        resolve_positional_params(design)?;
        bind_param_variants(design, resolver).map_err(ElabError::Message)?;
        match try_elaborate(design, top, register_pkg_funcs) {
            Ok(f) => return Ok(f),
            Err(ElabError::UnknownUnit { name, .. }) => {
                if tried.iter().any(|t| *t == name) {
                    return Err(ElabError::Message(format!(
                        "dependency cycle involving '{}'",
                        name
                    )));
                }
                tried.push(name.clone());
                if resolver
                    .resolve(design, &name)
                    .map_err(ElabError::Message)?
                {
                    continue;
                }
                return Err(ElabError::UnknownUnit {
                    name,
                    where_: "top".to_string(),
                });
            }
            Err(e) => return Err(e),
        }
    }
}

/// Rewrite instance module references to generic-map variants: for every
/// instance with constant `param_overrides` whose target module declares
/// params, ask the resolver for a variant module re-lowered with those
/// overrides and point the instance at it. Repeats to a fixpoint (variants
/// may carry their own instances with overrides).
/// Bind positional generic-map actuals (`generic map (v1, v2, …)`) by name:
/// the k-th positional actual maps to the target module's k-th parameter in
/// declaration order. Runs before variant binding — generic values select
/// the re-lowered variant, so overrides must carry names by then. Instances
/// whose target unit is not compiled yet keep their positionals until the
/// next pass round; config bindings applied during instantiation resolve in
/// `instantiate`, which has the final child module at hand.
fn resolve_positional_params(design: &mut Design) -> Result<(), ElabError> {
    for mi in 0..design.modules.len() {
        for ii in 0..design.modules[mi].instances.len() {
            let has_pos = design.modules[mi].instances[ii]
                .param_overrides
                .iter()
                .any(|(n, _)| n.is_none());
            if !has_pos {
                continue;
            }
            let unit = design.modules[mi].instances[ii].module.clone();
            let where_ = design.modules[mi].instances[ii].name.clone();
            let Some(tid) = design.modules.iter().position(|m| m.name == unit) else {
                continue;
            };
            if !design.modules[tid].is_vhdl {
                continue; // Verilog `#(…)` overrides are always named
            }
            let names: Vec<String> = design.modules[tid]
                .params
                .iter()
                .map(|p| p.name.clone())
                .collect();
            let inst = &mut design.modules[mi].instances[ii];
            let mut k = 0usize;
            for (n, _) in inst.param_overrides.iter_mut() {
                if n.is_some() {
                    continue;
                }
                match names.get(k) {
                    Some(name) => *n = Some(name.clone()),
                    None => {
                        return Err(ElabError::Message(format!(
                            "{}: more positional generic-map actuals than '{}' has generics ({})",
                            if where_.is_empty() {
                                design.modules[mi].name.clone()
                            } else {
                                format!("{}.{}", design.modules[mi].name, where_)
                            },
                            unit,
                            names.len()
                        )))
                    }
                }
                k += 1;
            }
        }
    }
    Ok(())
}

fn bind_param_variants(design: &mut Design, resolver: &mut dyn UnitResolver) -> Result<(), String> {
    loop {
        let mut changed = false;
        for mi in 0..design.modules.len() {
            for ii in 0..design.modules[mi].instances.len() {
                if design.modules[mi].instances[ii].param_overrides.is_empty() {
                    continue;
                }
                let unit = design.modules[mi].instances[ii].module.clone();
                let Some(tid) = design.modules.iter().position(|m| m.name == unit) else {
                    continue;
                };
                // Only VHDL entities can be re-lowered as generic-map variants.
                if !design.modules[tid].is_vhdl || design.modules[tid].params.is_empty() {
                    continue;
                }
                let mut params: HashMap<String, Value> = HashMap::new();
                for (n, e) in &design.modules[mi].instances[ii].param_overrides {
                    let Some(n) = n else { continue }; // resolved by resolve_positional_params
                    if let Expr::Lit(v) = e {
                        // An X-valued actual carries no binding information:
                        // it is what an initializer fell back to when it
                        // could not be const-folded. Keying a variant on it
                        // duplicates the whole entity/architecture lowering
                        // (and its diagnostics) for a garbage key — skip it
                        // so the instance shares the base (or a variant
                        // keyed by the real-valued params only).
                        if v.is_known() {
                            params.insert(n.to_ascii_lowercase(), v.clone());
                        }
                    }
                }
                if params.is_empty() {
                    continue;
                }
                if let Some(v) = resolver.resolve_variant(design, &unit, &params, tid)? {
                    if v != unit {
                        design.modules[mi].instances[ii].module = v;
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            return Ok(());
        }
    }
}

fn try_elaborate(design: &Design, top: &str, register_pkg_funcs: bool) -> Result<Flat, ElabError> {
    // The top may be a configuration declaration: elaborate its bound entity
    // with the configuration's block bindings applied at the top scope.
    let (top_id, top_cfg) = if let Some(pos) = design.modules.iter().position(|m| m.name == top) {
        (pos, None)
    } else if let Some(c) = design.configurations.get(&top.to_ascii_lowercase()) {
        let ent = c.entity.clone();
        let pos = design
            .modules
            .iter()
            .position(|m| m.name == ent || m.name.starts_with(&format!("{ent}/")))
            .ok_or_else(|| {
                ElabError::Message(format!(
                    "configuration '{top}' binds unknown entity '{ent}'"
                ))
            })?;
        (pos, Some(top.to_ascii_lowercase()))
    } else {
        return Err(ElabError::Message(format!(
            "top module '{}' not found",
            top
        )));
    };
    let mut f = Flat::default();
    let root = f.new_scope("", None);
    f.top_scope = root;
    f.instantiate(
        design,
        top_id,
        root,
        "",
        &HashMap::new(),
        &[],
        top_cfg.as_deref(),
    )?;
    // Package function bodies (visible via `use lib.pkg.all;`) are callable
    // at runtime too — register them like module functions. Bodies live in
    // slot space, so they are cloned without remapping. Unbound generic
    // packages (still carrying generics) are skipped: in VHDL they are only
    // usable through an instantiation, whose bound copy is registered under
    // its own name. Sorted order keeps the registration deterministic when
    // several packages export the same function name.
    if !register_pkg_funcs {
        dedup_functions(&mut f);
        return Ok(f);
    }
    let mut pkg_names: Vec<&String> = design.packages.keys().collect();
    pkg_names.sort();
    for pn in pkg_names {
        let p = &design.packages[pn];
        if !p.generics.is_empty() {
            continue;
        }
        for func in &p.functions {
            let mut slots: Vec<usize> = vec![func.ret_width.max(1)];
            slots.extend(func.args.iter().map(|(_, w, _)| *w));
            slots.extend(func.locals.iter().map(|l| l.width));
            f.functions.push(FlatFunc {
                name: func.name.clone(),
                ret_width: func.ret_width,
                arity: func.args.len(),
                slots,
                formal_types: func.args.iter().map(|(_, _, t)| t.clone()).collect(),
                defaults: func.defaults.clone(),
                body: Arc::new(func.body.clone()),
            });
        }
    }
    dedup_functions(&mut f);
    Ok(f)
}

/// A lowered-but-empty function body: the interface-only form a package-spec
/// function lowers to before its package body is seen.
fn func_body_is_stub(f: &FlatFunc) -> bool {
    match &*f.body {
        Stmt::Null => true,
        Stmt::Block(ss) => ss.is_empty(),
        _ => false,
    }
}

/// Package-spec functions and their package-body definitions lower as
/// separate design units, so the interface-only stub and the real body both
/// reach this list. The body wins (matching the simulator's overload table,
/// where a later same-signature registration replaces the earlier one), and
/// the interface defaults recorded on the spec (`w : natural := 8`) carry
/// over when the body's interface omits them. A real body replaces an
/// interface-only stub; two real bodies with the same formal widths are
/// genuine overloads and coexist (dispatch separates them by actual width
/// where it can, and the recursion deviation covers the rest).
fn dedup_functions(f: &mut Flat) {
    let same_sig = |a: &FlatFunc, b: &FlatFunc| {
        a.name.eq_ignore_ascii_case(&b.name)
            && a.arity == b.arity
            && a.slots[1..=a.arity] == b.slots[1..=b.arity]
    };
    let mut merged: Vec<FlatFunc> = Vec::with_capacity(f.functions.len());
    let has_defaults = |d: &[Option<Value>]| d.iter().any(|d| d.is_some());
    for func in std::mem::take(&mut f.functions) {
        match merged.iter().position(|m| same_sig(m, &func)) {
            Some(i) => {
                let mut func = func;
                // Defaults: whichever side recorded them wins.
                if !has_defaults(&func.defaults) {
                    func.defaults = merged[i].defaults.clone();
                }
                // A real body replaces an interface-only stub; a stub never
                // displaces a real body. Two REAL bodies with the same
                // formal widths are genuine overloads the width model
                // cannot separate (`conj(complex_t)` vs
                // `conj(complex_float_array)`, both w194) — they must
                // coexist, or element-wise recursion inside one of them
                // resolves back to itself and never terminates.
                if func_body_is_stub(&merged[i]) {
                    merged[i] = func;
                } else if !func_body_is_stub(&func) {
                    merged.push(func);
                }
            }
            None => merged.push(func),
        }
    }
    f.functions = merged;
}

impl Flat {
    fn new_scope(&mut self, name: &str, parent: Option<ScopeId>) -> ScopeId {
        let id = self.scopes.len() as ScopeId;
        self.scopes.push(Scope {
            name: name.to_string(),
            parent,
        });
        id
    }

    fn new_sig(
        &mut self,
        name: String,
        width: usize,
        kind: SigKind,
        init: Option<Value>,
        scope: ScopeId,
        signed: bool,
    ) -> FlatSigId {
        let id = self.signals.len() as FlatSigId;
        self.signals.push(FlatSig {
            name,
            width,
            kind,
            init,
            mem: None,
            signed,
            scope,
            real_ranges: Vec::new(),
        });
        id
    }

    /// Instantiate one module. `conns` are the parent's port connections.
    /// Returns the signal map (module sig id → flat sig id) for this instance.
    fn instantiate(
        &mut self,
        design: &Design,
        mod_id: usize,
        parent_scope: ScopeId,
        prefix: &str,
        params: &HashMap<String, Value>,
        conns: &[Conn],
        cfg: Option<&str>,
    ) -> Result<Vec<FlatSigId>, ElabError> {
        let m = &design.modules[mod_id];
        // A configuration bound to this instance rewrites the child instance
        // list (block configurations) before recursion.
        let child_instances: Vec<InstIr> = match cfg.and_then(|c| design.configurations.get(c)) {
            Some(c) => apply_config_blocks(c, &m.instances),
            None => m.instances.clone(),
        };
        let where_ = if prefix.is_empty() {
            m.name.clone()
        } else {
            prefix.to_string()
        };

        // Resolve parameters: defaults overridden by `params`.
        let mut param_vals: HashMap<String, Value> = HashMap::new();
        for p in &m.params {
            let v = params
                .get(&p.name)
                .cloned()
                .unwrap_or_else(|| p.default.clone());
            param_vals.insert(p.name.clone(), v);
        }

        let scope = self.new_scope(&m.name, Some(parent_scope));

        // Create flat signals for all non-port module signals. Port signals are
        // not created here: they are aliased to (or bridged with) the parent's
        // connection during port connection below.
        let port_sigs: std::collections::HashSet<usize> =
            m.ports.iter().map(|p| p.sig as usize).collect();
        let mut map: Vec<FlatSigId> = Vec::with_capacity(m.signals.len());
        for (i, s) in m.signals.iter().enumerate() {
            if port_sigs.contains(&i) {
                map.push(FlatSigId::MAX); // placeholder, filled by port connection
                continue;
            }
            let name = if prefix.is_empty() {
                s.name.clone()
            } else {
                format!("{}.{}", prefix, s.name)
            };
            let id = self.new_sig(name, s.width, s.kind, s.init.clone(), scope, s.signed);
            self.signals[id as usize].mem = s.mem;
            // Real-typed record fields decode as IEEE-754 in the simulator.
            if let Some(r) = m.real_ranges.get(&s.name) {
                self.signals[id as usize].real_ranges = r.clone();
            }
            map.push(id);
        }

        // Connect ports. `conns` are already remapped to flat (parent) signal
        // ids by the caller, so they must NOT be remapped through the child map.
        for (i, port) in m.ports.iter().enumerate() {
            match conn_for(conns, &m.ports, i) {
                Some(expr) => {
                    let expr = expr.clone();
                    match (&expr, port.dir) {
                        (Expr::Sig(ps), _) => {
                            // Alias: the child port *is* the parent signal.
                            map[port.sig as usize] = *ps;
                        }
                        _ => {
                            let imp_name = format!("{}.{}", where_, port.name);
                            let imp = self.new_sig(
                                imp_name,
                                m.signals[port.sig as usize].width.max(1),
                                port_dir_kind(port.dir),
                                None,
                                scope,
                                m.signals[port.sig as usize].signed,
                            );
                            // Record-typed port bridged by an expression: keep
                            // the real-field layout for field selects.
                            if let Some(r) = m.real_ranges.get(&m.signals[port.sig as usize].name) {
                                self.signals[imp as usize].real_ranges = r.clone();
                            }
                            map[port.sig as usize] = imp;
                            match port.dir {
                                Dir::In => self.assigns.push(FlatAssign {
                                    lhs: Expr::Sig(imp),
                                    rhs: expr,
                                }),
                                // A literal actual on an out/inout port (e.g. an
                                // explicit `open`) is not an lvalue: leave the
                                // port unconnected instead of assigning to it.
                                Dir::Out | Dir::Inout => {
                                    if !matches!(expr, Expr::Lit(_) | Expr::Real(_)) {
                                        self.assigns.push(FlatAssign {
                                            lhs: expr,
                                            rhs: Expr::Sig(imp),
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
                // Unassociated port (`open`, or omitted from the port map) —
                // legal VHDL/Verilog. Give the child a private signal,
                // initialised to the port's declared default when it has one;
                // out/inout ports simply stay undriven.
                None => {
                    let psig = &m.signals[port.sig as usize];
                    let imp_name = format!("{}.{}", where_, port.name);
                    let imp = self.new_sig(
                        imp_name,
                        psig.width.max(1),
                        port_dir_kind(port.dir),
                        psig.init.clone(),
                        scope,
                        psig.signed,
                    );
                    self.signals[imp as usize].mem = psig.mem;
                    map[port.sig as usize] = imp;
                }
            }
        }

        // Processes and assigns.
        for p in &m.processes {
            let body = remap_stmt(&p.body, &map);
            let sens = match &p.sens {
                Sens::List(es) => Sens::List(
                    es.iter()
                        .map(|e| Edge {
                            sig: map[e.sig as usize],
                            kind: e.kind,
                        })
                        .collect(),
                ),
                Sens::All => Sens::All,
            };
            self.procs.push(FlatProc {
                kind: p.kind,
                sens,
                body,
                origin: p.origin.clone(),
                initial: p.initial,
            });
        }
        for a in &m.assigns {
            self.assigns.push(FlatAssign {
                lhs: remap_expr(&a.lhs, &map),
                rhs: remap_expr(&a.rhs, &map),
            });
        }
        for f in &m.functions {
            // Slot 0 is the function name (return value), then args, then locals.
            let mut slots: Vec<usize> = vec![f.ret_width.max(1)];
            slots.extend(f.args.iter().map(|(_, w, _)| *w));
            let arg_ids: Vec<SigId> = (0..f.args.len() as SigId).collect();
            let local_base = f.args.len() as SigId;
            for l in &f.locals {
                slots.push(l.width);
            }
            // Function bodies live in slot space (args then locals); the lowerer
            // already emitted slot ids, so do NOT remap them through the module map.
            let body = Arc::new(f.body.clone());
            let _ = (arg_ids, local_base);
            self.functions.push(FlatFunc {
                name: f.name.clone(),
                ret_width: f.ret_width,
                arity: f.args.len(),
                slots,
                formal_types: f.args.iter().map(|(_, _, t)| t.clone()).collect(),
                defaults: f.defaults.clone(),
                body,
            });
        }

        // Child instances, recursively.
        for inst in &child_instances {
            let child_id = design
                .modules
                .iter()
                .position(|mm| mm.name == inst.module)
                .ok_or_else(|| ElabError::UnknownUnit {
                    name: inst.module.clone(),
                    where_: where_.clone(),
                })?;
            let child_prefix = if prefix.is_empty() {
                inst.name.clone()
            } else {
                format!("{}.{}", prefix, inst.name)
            };
            // Parameter overrides as values where constant. Positional
            // actuals (config bindings can inject them after the pre-pass)
            // bind to the child's parameters in declaration order.
            let mut child_params = param_vals.clone();
            let mut pos = 0usize;
            for (pname, pexpr) in &inst.param_overrides {
                if pname.is_none() {
                    pos += 1; // every positional actual consumes a formal slot
                }
                let e = remap_expr(pexpr, &map);
                if let Expr::Lit(v) = e {
                    match pname {
                        Some(n) => {
                            child_params.insert(n.clone(), v);
                        }
                        None => {
                            if let Some(p) = m.params.get(pos - 1) {
                                child_params.insert(p.name.clone(), v);
                            }
                        }
                    }
                }
            }
            let child_conns: Vec<Conn> = inst
                .conns
                .iter()
                .map(|c| match c {
                    Conn::Named(n, e) => Conn::Named(n.clone(), remap_expr(e, &map)),
                    Conn::Positional(e) => Conn::Positional(remap_expr(e, &map)),
                })
                .collect();
            self.instantiate(
                design,
                child_id,
                scope,
                &child_prefix,
                &child_params,
                &child_conns,
                inst.config.as_deref(),
            )?;
        }

        Ok(map)
    }
}

/// Apply a configuration's block bindings to an instance list: `for label :
/// comp use ...`, `for all : comp use ...`, `for others : comp use ...`.
fn apply_config_blocks(cfg: &crate::vhdl_lower::ConfigIr, instances: &[InstIr]) -> Vec<InstIr> {
    use crate::vhdl_lower::Binding;
    let mut out: Vec<InstIr> = instances.to_vec();
    let mut named_matched: Vec<usize> = Vec::new();
    let apply = |inst: &mut InstIr, blk: &crate::vhdl_lower::BlockCfg| match &blk.binding {
        Binding::Entity {
            name,
            arch,
            generic_map,
            port_map,
        } => {
            inst.module = match arch {
                Some(a) => format!("{name}/{a}"),
                None => name.clone(),
            };
            inst.param_overrides.extend(generic_map.clone());
            if !port_map.is_empty() {
                inst.conns = port_map.clone();
            }
        }
        Binding::Configuration { name } => {
            inst.config = Some(name.clone());
        }
    };
    for blk in &cfg.blocks {
        if blk.others {
            continue; // after the named blocks
        }
        for (i, inst) in out.iter_mut().enumerate() {
            let hit = if blk.all {
                true
            } else {
                !blk.path.is_empty()
                    && blk.path[0].eq_ignore_ascii_case(&inst.name)
                    && (blk.path.len() == 1 || blk.path[1].eq_ignore_ascii_case(&inst.module))
            };
            if hit {
                named_matched.push(i);
                apply(inst, blk);
            }
        }
    }
    for blk in &cfg.blocks {
        if !blk.others {
            continue;
        }
        for (i, inst) in out.iter_mut().enumerate() {
            if named_matched.contains(&i) {
                continue;
            }
            if blk.path.len() >= 2 && !blk.path[1].eq_ignore_ascii_case(&inst.module) {
                continue;
            }
            apply(inst, blk);
        }
    }
    out
}

fn conn_for<'a>(conns: &'a [Conn], ports: &[PortIr], i: usize) -> Option<&'a Expr> {
    // Named connections match by name; positional fill in order.
    let mut positional = 0;
    for c in conns {
        match c {
            Conn::Named(n, e) => {
                if ports.get(i).map_or(false, |p| p.name == *n) {
                    return Some(e);
                }
            }
            Conn::Positional(e) => {
                if positional == i {
                    return Some(e);
                }
                positional += 1;
            }
        }
    }
    None
}

fn port_dir_kind(d: Dir) -> SigKind {
    match d {
        Dir::In => SigKind::Input,
        Dir::Out => SigKind::Output,
        Dir::Inout => SigKind::Inout,
    }
}

/// Remap all signal ids in an expression through `map`.
pub fn remap_expr(e: &Expr, map: &[FlatSigId]) -> Expr {
    match e {
        Expr::Lit(v) => Expr::Lit(v.clone()),
        Expr::Real(x) => Expr::Real(*x),
        Expr::Sig(s) => Expr::Sig(map[*s as usize]),
        Expr::Bin { op, l, r } => Expr::Bin {
            op: *op,
            l: Box::new(remap_expr(l, map)),
            r: Box::new(remap_expr(r, map)),
        },
        Expr::Un { op, e } => Expr::Un {
            op: *op,
            e: Box::new(remap_expr(e, map)),
        },
        Expr::Cond { c, t, e } => Expr::Cond {
            c: Box::new(remap_expr(c, map)),
            t: Box::new(remap_expr(t, map)),
            e: Box::new(remap_expr(e, map)),
        },
        Expr::Call { name, args } => Expr::Call {
            name: name.clone(),
            args: args.iter().map(|a| remap_expr(a, map)).collect(),
        },
        Expr::Concat(parts) => Expr::Concat(parts.iter().map(|p| remap_expr(p, map)).collect()),
        Expr::Replicate { count, e } => Expr::Replicate {
            count: Box::new(remap_expr(count, map)),
            e: Box::new(remap_expr(e, map)),
        },
        Expr::Rising(e) => Expr::Rising(Box::new(remap_expr(e, map))),
        Expr::Prev(e) => Expr::Prev(Box::new(remap_expr(e, map))),
        Expr::Falling(e) => Expr::Falling(Box::new(remap_expr(e, map))),
        Expr::ToInt(e) => Expr::ToInt(Box::new(remap_expr(e, map))),
        Expr::ToString(e) => Expr::ToString(Box::new(remap_expr(e, map))),
        Expr::Resize { e, width, signed } => Expr::Resize {
            e: Box::new(remap_expr(e, map)),
            width: *width,
            signed: *signed,
        },
        Expr::Signed(e) => Expr::Signed(Box::new(remap_expr(e, map))),
        Expr::SigAttr { sig, attr } => Expr::SigAttr {
            sig: map[*sig as usize],
            attr: *attr,
        },
        Expr::Sel { e, msb, lsb } => Expr::Sel {
            e: Box::new(remap_expr(e, map)),
            msb: Box::new(remap_expr(msb, map)),
            lsb: Box::new(remap_expr(lsb, map)),
        },
        Expr::Idx { e, i } => Expr::Idx {
            e: Box::new(remap_expr(e, map)),
            i: Box::new(remap_expr(i, map)),
        },
    }
}

pub fn remap_stmt(s: &Stmt, map: &[FlatSigId]) -> Stmt {
    match s {
        Stmt::Spanned { span, inner } => Stmt::Spanned {
            span: *span,
            inner: Box::new(remap_stmt(inner, map)),
        },
        Stmt::Block(ss) => Stmt::Block(ss.iter().map(|s| remap_stmt(s, map)).collect()),
        Stmt::Return(e) => Stmt::Return(Box::new(remap_expr(e, map))),
        Stmt::If { cond, then, els } => Stmt::If {
            cond: remap_expr(cond, map),
            then: Box::new(remap_stmt(then, map)),
            els: els.as_ref().map(|e| Box::new(remap_stmt(e, map))),
        },
        Stmt::Case {
            expr,
            items,
            default,
            matching,
        } => Stmt::Case {
            expr: remap_expr(expr, map),
            items: items
                .iter()
                .map(|(cs, b)| {
                    (
                        cs.iter().map(|c| remap_expr(c, map)).collect(),
                        remap_stmt(b, map),
                    )
                })
                .collect(),
            default: default.as_ref().map(|d| Box::new(remap_stmt(d, map))),
            matching: *matching,
        },
        Stmt::Assign { lhs, rhs, nba } => Stmt::Assign {
            lhs: remap_expr(lhs, map),
            rhs: remap_expr(rhs, map),
            nba: *nba,
        },
        Stmt::AssignDelayed { lhs, rhs, delay } => Stmt::AssignDelayed {
            lhs: remap_expr(lhs, map),
            rhs: remap_expr(rhs, map),
            delay: remap_expr(delay, map),
        },
        Stmt::Delay { time_fs, stmt } => Stmt::Delay {
            time_fs: *time_fs,
            stmt: Box::new(remap_stmt(stmt, map)),
        },
        Stmt::Wait { sens, body } => Stmt::Wait {
            sens: sens
                .iter()
                .map(|e| Edge {
                    sig: map[e.sig as usize],
                    kind: e.kind,
                })
                .collect(),
            body: Box::new(remap_stmt(body, map)),
        },
        Stmt::WaitUntil {
            cond,
            sens,
            timeout,
        } => Stmt::WaitUntil {
            cond: cond.as_ref().map(|c| remap_expr(c, map)),
            sens: sens
                .iter()
                .map(|e| Edge {
                    sig: map[e.sig as usize],
                    kind: e.kind,
                })
                .collect(),
            timeout: timeout.as_ref().map(|t| remap_expr(t, map)),
        },
        Stmt::SysCall { name, args } => Stmt::SysCall {
            name: name.clone(),
            args: args.iter().map(|a| remap_expr(a, map)).collect(),
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
        } => Stmt::For {
            init: Box::new(remap_stmt(init, map)),
            cond: remap_expr(cond, map),
            step: Box::new(remap_stmt(step, map)),
            body: Box::new(remap_stmt(body, map)),
        },
        Stmt::Repeat { count, body } => Stmt::Repeat {
            count: remap_expr(count, map),
            body: Box::new(remap_stmt(body, map)),
        },
        Stmt::Break => Stmt::Break,
        Stmt::Continue => Stmt::Continue,
        Stmt::ExprStmt(e) => Stmt::ExprStmt(remap_expr(e, map)),
        Stmt::Null => Stmt::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(name: &str, w: usize, kind: SigKind) -> SignalIr {
        SignalIr {
            name: name.to_string(),
            width: w,
            kind,
            init: None,
            mem: None,
            signed: false,
        }
    }

    /// top instantiates child; child.q is an output connected to top's wire w.
    fn two_module_design() -> Design {
        let mut child = ModuleIr {
            name: "child".into(),
            params: vec![],
            ports: vec![PortIr {
                name: "q".into(),
                dir: Dir::Out,
                sig: 0,
            }],
            signals: vec![sig("q", 8, SigKind::Output)],
            assigns: vec![],
            processes: vec![],
            functions: vec![],
            instances: vec![],
            time_unit_fs: 1000,
            is_vhdl: false,
            real_ranges: HashMap::new(),
            sig_dims_debug: None,
        };
        child.assigns.push(AssignIr {
            lhs: Expr::Sig(0),
            rhs: Expr::Lit(Value::from_u64(8, 42)),
        });
        let top = ModuleIr {
            name: "top".into(),
            params: vec![],
            ports: vec![],
            signals: vec![sig("w", 8, SigKind::Wire)],
            assigns: vec![],
            processes: vec![],
            functions: vec![],
            instances: vec![InstIr {
                module: "child".into(),
                name: "u0".into(),
                param_overrides: vec![],
                conns: vec![Conn::Named("q".into(), Expr::Sig(0))],
                config: None,
            }],
            time_unit_fs: 1000,
            is_vhdl: false,
            real_ranges: HashMap::new(),
            sig_dims_debug: None,
        };
        Design {
            modules: vec![top, child],
            top: Some("top".into()),
            packages: Default::default(),
            pkg_funcs: Default::default(),
            configurations: Default::default(),
        }
    }

    #[test]
    fn configuration_as_top_applies_block_bindings() {
        // top instantiates "child" as u0; `configuration cfg of top` binds
        // `for u0 : child use entity work.other(rtl);` — elaborating the
        // configuration as top must instantiate other/rtl under u0.
        use crate::vhdl_lower::{Binding, BlockCfg, ConfigIr};
        let mut d = two_module_design();
        let mut other = ModuleIr {
            name: "other/rtl".into(),
            params: vec![],
            ports: vec![PortIr {
                name: "q".into(),
                dir: Dir::Out,
                sig: 0,
            }],
            signals: vec![sig("q", 8, SigKind::Output)],
            assigns: vec![],
            processes: vec![],
            functions: vec![],
            instances: vec![],
            time_unit_fs: 1000,
            is_vhdl: false,
            real_ranges: HashMap::new(),
            sig_dims_debug: None,
        };
        other.assigns.push(AssignIr {
            lhs: Expr::Sig(0),
            rhs: Expr::Lit(Value::from_u64(8, 7)),
        });
        d.modules.push(other);
        d.configurations.insert(
            "cfg".into(),
            std::rc::Rc::new(ConfigIr {
                name: "cfg".into(),
                entity: "top".into(),
                arch: None,
                blocks: vec![BlockCfg {
                    path: vec!["u0".into(), "child".into()],
                    all: false,
                    others: false,
                    binding: Binding::Entity {
                        name: "other".into(),
                        arch: Some("rtl".into()),
                        generic_map: vec![],
                        port_map: vec![],
                    },
                }],
            }),
        );
        let f = elaborate(&d, "cfg").unwrap();
        let scope_names: Vec<&str> = f.scopes.iter().map(|s| s.name.as_str()).collect();
        assert!(
            scope_names.contains(&"other/rtl"),
            "block binding not applied: {:?}",
            scope_names
        );
        assert!(!scope_names.contains(&"child"), "{:?}", scope_names);
    }

    #[test]
    fn elaborates_two_level_hierarchy() {
        let d = two_module_design();
        let f = elaborate(&d, "top").unwrap();
        // The child's output port is aliased to the parent's `w` — no separate
        // u0.q flat signal is created; the child's internal assign remaps to w.
        let names: Vec<&str> = f.signals.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["w"], "{:?}", names);
        assert_eq!(f.assigns.len(), 1);
        assert!(matches!(f.assigns[0].lhs, Expr::Sig(0)));
        assert!(f.scopes.len() >= 2);
    }

    #[test]
    fn unknown_top_is_an_error() {
        let d = two_module_design();
        assert!(elaborate(&d, "nope").is_err());
    }

    #[test]
    fn unknown_child_module_is_an_error() {
        let mut d = two_module_design();
        d.modules[1].name = "renamed".into();
        assert!(elaborate(&d, "top").is_err());
    }
}
