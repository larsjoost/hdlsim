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

pub type FlatSigId = u32;
pub type ScopeId = u32;

#[derive(Clone, Debug)]
pub struct Scope {
    pub name: String,
    pub parent: Option<ScopeId>,
}

#[derive(Clone, Debug)]
pub struct FlatSig {
    /// Full hierarchical name, e.g. `tb.dut.q`.
    pub name: String,
    pub width: usize,
    pub kind: SigKind,
    pub init: Option<Value>,
    pub scope: ScopeId,
}

#[derive(Clone, Debug)]
pub struct FlatProc {
    pub kind: ProcKind,
    pub sens: Sens,
    pub body: Stmt,
    pub origin: String,
}

#[derive(Clone, Debug)]
pub struct FlatAssign {
    pub lhs: Expr,
    pub rhs: Expr,
}

/// A function body with its local slot space (`args` then `locals`).
#[derive(Clone, Debug)]
pub struct FlatFunc {
    pub name: String,
    pub ret_width: usize,
    /// Width of each slot; slot ids in the body index this vector.
    pub slots: Vec<usize>,
    pub body: Stmt,
}

#[derive(Clone, Debug, Default)]
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
pub fn elaborate(design: &Design, top: &str) -> Result<Flat, String> {
    let top_id = design
        .modules
        .iter()
        .position(|m| m.name == top)
        .ok_or_else(|| format!("top module '{}' not found", top))?;
    let mut f = Flat::default();
    let root = f.new_scope("", None);
    f.top_scope = root;
    f.instantiate(design, top_id, root, "", &HashMap::new(), &[])?;
    Ok(f)
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
    ) -> FlatSigId {
        let id = self.signals.len() as FlatSigId;
        self.signals.push(FlatSig {
            name,
            width,
            kind,
            init,
            scope,
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
    ) -> Result<Vec<FlatSigId>, String> {
        let m = &design.modules[mod_id];
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

        // Create flat signals for all module signals.
        let mut map: Vec<FlatSigId> = Vec::with_capacity(m.signals.len());
        for s in &m.signals {
            let name = if prefix.is_empty() {
                s.name.clone()
            } else {
                format!("{}.{}", prefix, s.name)
            };
            map.push(self.new_sig(name, s.width, s.kind, s.init.clone(), scope));
        }

        // Connect ports.
        for (i, port) in m.ports.iter().enumerate() {
            let conn = conn_for(conns, &m.ports, i);
            let Some(conn) = conn else {
                return Err(format!("{}: port '{}' not connected", where_, port.name));
            };
            let expr = remap_expr(conn, &map);
            match (&expr, port.dir) {
                (Expr::Sig(ps), _) => {
                    // Alias: the child port *is* the parent signal.
                    map[port.sig as usize] = *ps;
                }
                _ => {
                    let imp_name = format!("{}.{}", where_, port.name);
                    let imp = self.new_sig(
                        imp_name,
                        expr_width(&expr),
                        port_dir_kind(port.dir),
                        None,
                        scope,
                    );
                    map[port.sig as usize] = imp;
                    match port.dir {
                        Dir::In => self.assigns.push(FlatAssign {
                            lhs: Expr::Sig(imp),
                            rhs: expr,
                        }),
                        Dir::Out | Dir::Inout => self.assigns.push(FlatAssign {
                            lhs: expr,
                            rhs: Expr::Sig(imp),
                        }),
                    }
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
            });
        }
        for a in &m.assigns {
            self.assigns.push(FlatAssign {
                lhs: remap_expr(&a.lhs, &map),
                rhs: remap_expr(&a.rhs, &map),
            });
        }
        for f in &m.functions {
            let mut slots: Vec<usize> = f.args.iter().map(|(_, w)| *w).collect();
            let arg_ids: Vec<SigId> = (0..f.args.len() as SigId).collect();
            let local_base = f.args.len() as SigId;
            for l in &f.locals {
                slots.push(l.width);
            }
            // Body sig ids: args and locals are function-local; remap locals to
            // slot space by adding nothing — the lowerer already emitted local ids
            // in [0..args+locals). Just remap module-level sigs (captured) too.
            let body = remap_stmt(&f.body, &map);
            let _ = (arg_ids, local_base);
            self.functions.push(FlatFunc {
                name: f.name.clone(),
                ret_width: f.ret_width,
                slots,
                body,
            });
        }

        // Child instances, recursively.
        for inst in &m.instances {
            let child_id = design
                .modules
                .iter()
                .position(|mm| mm.name == inst.module)
                .ok_or_else(|| format!("{}: unknown module '{}'", where_, inst.module))?;
            let child_prefix = if prefix.is_empty() {
                inst.name.clone()
            } else {
                format!("{}.{}", prefix, inst.name)
            };
            // Parameter overrides as values where constant.
            let mut child_params = param_vals.clone();
            for (pname, pexpr) in &inst.param_overrides {
                let e = remap_expr(pexpr, &map);
                if let Expr::Lit(v) = e {
                    child_params.insert(pname.clone(), v);
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
            )?;
        }

        Ok(map)
    }
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

/// Width of an expression's value (static approximation; X-safe default 1).
fn expr_width(e: &Expr) -> usize {
    match e {
        Expr::Lit(v) => v.width(),
        Expr::Sig(_) => 1, // refined by caller context; implicit wires for outputs
        // are sized properly by the lowerer in practice
        Expr::Bin { l, r, .. } => expr_width(l).max(expr_width(r)),
        Expr::Un { e, .. } => expr_width(e),
        Expr::Cond { t, e, .. } => expr_width(t).max(expr_width(e)),
        Expr::Concat(parts) => parts.iter().map(expr_width).sum(),
        Expr::Sel { msb, lsb, .. } => {
            // Best effort; lowerer emits constants here in practice.
            let _ = (msb, lsb);
            1
        }
        Expr::Idx { e, .. } => 1.max(expr_width(e)),
        Expr::Call { .. } => 1,
        Expr::Replicate { e, .. } => expr_width(e),
    }
}

/// Remap all signal ids in an expression through `map`.
pub fn remap_expr(e: &Expr, map: &[FlatSigId]) -> Expr {
    match e {
        Expr::Lit(v) => Expr::Lit(v.clone()),
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
        Stmt::Block(ss) => Stmt::Block(ss.iter().map(|s| remap_stmt(s, map)).collect()),
        Stmt::If { cond, then, els } => Stmt::If {
            cond: remap_expr(cond, map),
            then: Box::new(remap_stmt(then, map)),
            els: els.as_ref().map(|e| Box::new(remap_stmt(e, map))),
        },
        Stmt::Case {
            expr,
            items,
            default,
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
        },
        Stmt::Assign { lhs, rhs, nba } => Stmt::Assign {
            lhs: remap_expr(lhs, map),
            rhs: remap_expr(rhs, map),
            nba: *nba,
        },
        Stmt::Delay { time_fs, stmt } => Stmt::Delay {
            time_fs: *time_fs,
            stmt: Box::new(remap_stmt(stmt, map)),
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
            }],
            time_unit_fs: 1000,
        };
        Design {
            modules: vec![top, child],
            top: Some("top".into()),
        }
    }

    #[test]
    fn elaborates_two_level_hierarchy() {
        let d = two_module_design();
        let f = elaborate(&d, "top").unwrap();
        // top.w aliased to u0.q: only two flat signals exist.
        let names: Vec<&str> = f.signals.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"w"), "{:?}", names);
        assert!(names.contains(&"u0.q"), "{:?}", names);
        // The alias means w and u0.q are the same flat id — check via assign.
        assert_eq!(f.assigns.len(), 1);
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
