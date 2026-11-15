//! Static design heuristics — backs `hdlsim analyze`.
//!
//! Pure IR analysis, no simulation: for every module in the design, find
//! signals that are never driven (W0501) or never read (W0502). Ports count
//! as driven/read from the outside according to their direction, so an input
//! is only reported when nothing inside reads it, and an output only when
//! nothing inside drives it. Findings are lint-style codes, explained by
//! `hdlsim explain`.

use crate::ir::{Conn, Design, Dir, Expr, ModuleIr, Sens, SigId, SigKind, Stmt};
use hdl_syntax::diagnostic::Severity;
use std::collections::HashSet;

/// One heuristic finding. No span: the IR does not keep declaration spans.
#[derive(Clone, Debug)]
pub struct Finding {
    pub code: &'static str,
    pub severity: Severity,
    pub module: String,
    pub signal: String,
    pub message: String,
}

impl Finding {
    /// `warning[W0501] counter.q: never driven`
    pub fn render(&self) -> String {
        format!(
            "{}[{}] {}.{}: {}",
            self.severity.name(),
            self.code,
            self.module,
            self.signal,
            self.message
        )
    }
}

fn read(e: &Expr, reads: &mut HashSet<SigId>) {
    let mut v = Vec::new();
    e.collect_sigs(&mut v);
    reads.extend(v);
}

/// Collect signal ids assigned by `lhs` (the root of Idx/Sel chains counts:
/// `mem[i] = x` drives `mem`).
fn lhs_roots(e: &Expr, out: &mut HashSet<SigId>) {
    match e {
        Expr::Sig(id) => {
            out.insert(*id);
        }
        Expr::Idx { e: x, .. } | Expr::Sel { e: x, .. } => lhs_roots(x, out),
        Expr::Concat(parts) => {
            for p in parts {
                lhs_roots(p, out);
            }
        }
        other => read(other, out),
    }
}

fn walk_stmt(s: &Stmt, driven: &mut HashSet<SigId>, reads: &mut HashSet<SigId>) {
    match s {
        Stmt::Block(ss) => {
            for s in ss {
                walk_stmt(s, driven, reads);
            }
        }
        Stmt::If { cond, then, els } => {
            read(cond, reads);
            walk_stmt(then, driven, reads);
            if let Some(e) = els {
                walk_stmt(e, driven, reads);
            }
        }
        Stmt::Case {
            expr,
            items,
            default,
            ..
        } => {
            read(expr, reads);
            for (pats, body) in items {
                for p in pats {
                    read(p, reads);
                }
                walk_stmt(body, driven, reads);
            }
            if let Some(d) = default {
                walk_stmt(d, driven, reads);
            }
        }
        Stmt::Assign { lhs, rhs, .. } => {
            lhs_roots(lhs, driven);
            read(rhs, reads);
        }
        Stmt::Delay { stmt, .. } => walk_stmt(stmt, driven, reads),
        Stmt::Wait { sens, body } => {
            for e in sens {
                reads.insert(e.sig);
            }
            walk_stmt(body, driven, reads);
        }
        Stmt::SysCall { args, .. } => {
            for a in args {
                read(a, reads);
            }
        }
        Stmt::For {
            init,
            cond,
            step,
            body,
        } => {
            walk_stmt(init, driven, reads);
            read(cond, reads);
            walk_stmt(step, driven, reads);
            walk_stmt(body, driven, reads);
        }
        Stmt::Repeat { count, body } => {
            read(count, reads);
            walk_stmt(body, driven, reads);
        }
        Stmt::ExprStmt(e) => read(e, reads),
        Stmt::Break | Stmt::Continue | Stmt::Null => {}
        Stmt::Spanned { inner, .. } => walk_stmt(inner, driven, reads),
    }
}

fn analyze_module(m: &ModuleIr) -> Vec<Finding> {
    let mut driven: HashSet<SigId> = HashSet::new();
    let mut reads: HashSet<SigId> = HashSet::new();
    for a in &m.assigns {
        lhs_roots(&a.lhs, &mut driven);
        read(&a.rhs, &mut reads);
    }
    for p in &m.processes {
        if let Sens::List(edges) = &p.sens {
            for e in edges {
                reads.insert(e.sig);
            }
        }
        walk_stmt(&p.body, &mut driven, &mut reads);
    }
    for f in &m.functions {
        walk_stmt(&f.body, &mut driven, &mut reads);
    }
    for inst in &m.instances {
        for c in &inst.conns {
            let e = match c {
                Conn::Named(_, e) => e,
                Conn::Positional(e) => e,
            };
            read(e, &mut reads);
        }
        for (_, e) in &inst.param_overrides {
            read(e, &mut reads);
        }
    }
    // Ports are driven/read from the outside per their direction.
    for p in &m.ports {
        match p.dir {
            Dir::In | Dir::Inout => {
                driven.insert(p.sig);
            }
            Dir::Out => {
                reads.insert(p.sig);
            }
        }
    }
    let mut out = Vec::new();
    for (i, s) in m.signals.iter().enumerate() {
        if s.kind == SigKind::Genvar {
            continue;
        }
        let id = i as SigId;
        if !driven.contains(&id) {
            out.push(Finding {
                code: "W0501",
                severity: Severity::Warning,
                module: m.name.clone(),
                signal: s.name.clone(),
                message: "signal is never driven (constant X)".to_string(),
            });
        }
        if !reads.contains(&id) {
            out.push(Finding {
                code: "W0502",
                severity: Severity::Warning,
                module: m.name.clone(),
                signal: s.name.clone(),
                message: "signal is never read".to_string(),
            });
        }
    }
    out
}

/// Run all heuristics over every module in the design. Deterministic order:
/// module order, then signal order.
pub fn analyze_design(design: &Design) -> Vec<Finding> {
    let mut out = Vec::new();
    for m in &design.modules {
        out.extend(analyze_module(m));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{AssignIr, Edge, EdgeKind, PortIr, ProcKind, ProcessIr, SignalIr};
    use crate::value::Value;

    fn sig(name: &str, width: usize, kind: SigKind) -> SignalIr {
        SignalIr {
            name: name.to_string(),
            width,
            kind,
            init: None,
            mem: None,
            signed: false,
        }
    }

    fn port(name: &str, dir: Dir, sig: SigId) -> PortIr {
        PortIr {
            name: name.to_string(),
            dir,
            sig,
        }
    }

    /// module m(input clk, output q); wires dead / unused_in / tmp.
    fn sample() -> ModuleIr {
        ModuleIr {
            name: "m".to_string(),
            params: vec![],
            ports: vec![port("clk", Dir::In, 0), port("q", Dir::Out, 1)],
            signals: vec![
                sig("clk", 1, SigKind::Input),
                sig("q", 8, SigKind::Output),
                sig("dead", 8, SigKind::Wire),
                sig("unused_in", 1, SigKind::Wire),
                sig("tmp", 8, SigKind::Wire),
            ],
            assigns: vec![],
            processes: vec![ProcessIr {
                kind: ProcKind::Always,
                sens: Sens::List(vec![Edge {
                    sig: 0,
                    kind: EdgeKind::Pos,
                }]),
                body: Stmt::Assign {
                    lhs: Expr::Sig(4),
                    rhs: Expr::Lit(Value::from_u64(8, 1)),
                    nba: false,
                },
                origin: "always @(posedge clk)".to_string(),
                initial: false,
            }],
            functions: vec![],
            instances: vec![],
            time_unit_fs: 1_000_000,
        }
    }

    #[test]
    fn finds_undriven_and_unread_signals() {
        let mut m = sample();
        // q <- tmp, so q is driven and tmp is read.
        m.assigns.push(AssignIr {
            lhs: Expr::Sig(1),
            rhs: Expr::Sig(4),
        });
        let design = Design {
            modules: vec![m],
            top: None,
        };
        let fs = analyze_design(&design);
        // dead: neither driven nor read; unused_in: neither driven nor read;
        // q: driven by the assign and read as an output port; tmp: driven by
        // the process and read by the assign; clk: input + in sens list.
        assert!(
            fs.iter().any(|f| f.code == "W0501" && f.signal == "dead"),
            "{:?}",
            fs
        );
        assert!(
            fs.iter().any(|f| f.code == "W0502" && f.signal == "dead"),
            "{:?}",
            fs
        );
        assert!(
            fs.iter()
                .any(|f| f.code == "W0501" && f.signal == "unused_in"),
            "{:?}",
            fs
        );
        assert!(
            fs.iter()
                .any(|f| f.code == "W0502" && f.signal == "unused_in"),
            "{:?}",
            fs
        );
        for s in ["clk", "q", "tmp"] {
            assert!(!fs.iter().any(|f| f.signal == s), "{:?}", fs);
        }
        assert_eq!(fs.len(), 4, "{:?}", fs);
    }

    #[test]
    fn clean_module_has_no_findings() {
        let mut m = sample();
        m.assigns.push(AssignIr {
            lhs: Expr::Sig(1),
            rhs: Expr::Sig(4),
        });
        // drive dead from unused_in, drive unused_in itself, read tmp in the body.
        m.assigns.push(AssignIr {
            lhs: Expr::Sig(2),
            rhs: Expr::Sig(3),
        });
        m.assigns.push(AssignIr {
            lhs: Expr::Sig(3),
            rhs: Expr::Lit(Value::from_u64(1, 0)),
        });
        m.processes[0].body = Stmt::Block(vec![
            Stmt::Assign {
                lhs: Expr::Sig(4),
                rhs: Expr::Sig(3),
                nba: false,
            },
            Stmt::ExprStmt(Expr::Bin {
                op: crate::value::BinOp::Add,
                l: Box::new(Expr::Sig(4)),
                r: Box::new(Expr::Sig(2)),
            }),
        ]);
        let design = Design {
            modules: vec![m],
            top: None,
        };
        assert!(analyze_design(&design).is_empty());
    }

    #[test]
    fn memory_indexed_write_counts_as_driven() {
        let mut m = sample();
        let mut mem = sig("mem", 8, SigKind::Reg);
        mem.mem = Some(256);
        m.signals.push(mem);
        m.assigns.push(AssignIr {
            lhs: Expr::Sig(1),
            rhs: Expr::Bin {
                op: crate::value::BinOp::Add,
                l: Box::new(Expr::Sig(4)),
                r: Box::new(Expr::Sig(5)),
            },
        });
        m.processes[0].body = Stmt::Assign {
            lhs: Expr::Idx {
                e: Box::new(Expr::Sig(5)),
                i: Box::new(Expr::Lit(Value::from_u64(32, 0))),
            },
            rhs: Expr::Lit(Value::from_u64(8, 7)),
            nba: false,
        };
        let design = Design {
            modules: vec![m],
            top: None,
        };
        let fs = analyze_design(&design);
        assert!(!fs.iter().any(|f| f.signal == "mem"), "{:?}", fs);
    }

    #[test]
    fn render_is_lint_style() {
        let f = Finding {
            code: "W0501",
            severity: Severity::Warning,
            module: "m".to_string(),
            signal: "dead".to_string(),
            message: "signal is never driven (constant X)".to_string(),
        };
        assert_eq!(
            f.render(),
            "warning[W0501] m.dead: signal is never driven (constant X)"
        );
    }
}
