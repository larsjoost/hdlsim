//! Coverage collection as an [`Observer`] — the kernel never changes for it.
//!
//! Three kinds, per `docs/plan.md` phase 2:
//! - **line**: every `Stmt::Spanned` span executed (universe = all spans in the
//!   IR), reported per file with missed line numbers;
//! - **branch**: if-then/else arms and case items (identified by the arm
//!   statement's span) executed;
//! - **toggle**: per (signal, bit), whether both 0 and 1 were observed.
//!
//! The collector is fed by [`SimEvent::StmtExec`] / [`SimEvent::SignalChange`].

use crate::events::{Observer, SimEvent};
use hdl_ir::value::{V0, V1};
use hdl_ir::{Design, Stmt};
use hdl_syntax::source::{FileId, SourceDb, Span};
use std::collections::{HashMap, HashSet};

/// Walk the IR: all statement spans (line universe) and all branch-arm spans
/// (if then/else, case items, case default).
pub fn coverage_universe(design: &Design) -> (HashSet<Span>, HashSet<Span>) {
    fn arm_span(s: &Stmt) -> Option<Span> {
        match s {
            Stmt::Spanned { span, .. } => Some(*span),
            _ => None,
        }
    }
    fn walk(s: &Stmt, lines: &mut HashSet<Span>, branches: &mut HashSet<Span>) {
        match s {
            Stmt::Spanned { span, inner } => {
                lines.insert(*span);
                walk(inner, lines, branches);
            }
            Stmt::Block(ss) => {
                for s in ss {
                    walk(s, lines, branches);
                }
            }
            Stmt::If { then, els, .. } => {
                if let Some(sp) = arm_span(then) {
                    branches.insert(sp);
                }
                if let Some(e) = els {
                    if let Some(sp) = arm_span(e) {
                        branches.insert(sp);
                    }
                }
                walk(then, lines, branches);
                if let Some(e) = els {
                    walk(e, lines, branches);
                }
            }
            Stmt::Case { items, default, .. } => {
                for (_, item) in items {
                    if let Some(sp) = arm_span(item) {
                        branches.insert(sp);
                    }
                    walk(item, lines, branches);
                }
                if let Some(d) = default {
                    if let Some(sp) = arm_span(d) {
                        branches.insert(sp);
                    }
                    walk(d, lines, branches);
                }
            }
            Stmt::Delay { stmt, .. } | Stmt::Wait { body: stmt, .. } => walk(stmt, lines, branches),
            Stmt::For {
                init, step, body, ..
            } => {
                walk(init, lines, branches);
                walk(step, lines, branches);
                walk(body, lines, branches);
            }
            Stmt::Repeat { body, .. } => walk(body, lines, branches),
            Stmt::Break | Stmt::Continue => {}
            Stmt::Assign { .. } | Stmt::ExprStmt(_) | Stmt::Null | Stmt::SysCall { .. } => {}
        }
    }
    let mut lines = HashSet::new();
    let mut branches = HashSet::new();
    for m in &design.modules {
        for p in &m.processes {
            walk(&p.body, &mut lines, &mut branches);
        }
        for f in &m.functions {
            walk(&f.body, &mut lines, &mut branches);
        }
    }
    (lines, branches)
}

/// Collects coverage during a run; call [`CoverageCollector::report`] after
/// `Simulator::run` returns.
pub struct CoverageCollector {
    lines_total: HashSet<Span>,
    branches_total: HashSet<Span>,
    lines_hit: HashSet<Span>,
    branches_hit: HashSet<Span>,
    /// (sig, bit) -> (seen 0, seen 1)
    toggle: HashMap<(u32, usize), (bool, bool)>,
}

impl CoverageCollector {
    /// `sig_widths` are the flat (post-elaboration) signal widths, indexed by
    /// flat signal id — used for the toggle universe.
    pub fn new(design: &Design, sig_widths: &[usize]) -> CoverageCollector {
        let (lines_total, branches_total) = coverage_universe(design);
        let mut toggle = HashMap::new();
        for (sig, w) in sig_widths.iter().enumerate() {
            for b in 0..*w {
                toggle.insert((sig as u32, b), (false, false));
            }
        }
        CoverageCollector {
            lines_total,
            branches_total,
            lines_hit: HashSet::new(),
            branches_hit: HashSet::new(),
            toggle,
        }
    }

    pub fn line_coverage(&self) -> (usize, usize) {
        (self.lines_hit.len(), self.lines_total.len())
    }

    pub fn branch_coverage(&self) -> (usize, usize) {
        (self.branches_hit.len(), self.branches_total.len())
    }

    pub fn toggle_coverage(&self) -> (usize, usize) {
        let total = self.toggle.len();
        let hit = self.toggle.values().filter(|(z, o)| *z && *o).count();
        (hit, total)
    }

    /// Human-readable report: summary line + per-file missed lines.
    pub fn report(&self, db: &SourceDb) -> String {
        let (lh, lt) = self.line_coverage();
        let (bh, bt) = self.branch_coverage();
        let (th, tt) = self.toggle_coverage();
        let pct = |h: usize, t: usize| {
            if t == 0 {
                100.0
            } else {
                100.0 * h as f64 / t as f64
            }
        };
        let mut out = format!(
            "coverage: line {}/{} ({:.1}%), branch {}/{} ({:.1}%), toggle {}/{} ({:.1}%)\n",
            lh,
            lt,
            pct(lh, lt),
            bh,
            bt,
            pct(bh, bt),
            th,
            tt,
            pct(th, tt)
        );
        // Missed lines per file.
        let mut missed: HashMap<FileId, Vec<u32>> = HashMap::new();
        for span in self.lines_total.difference(&self.lines_hit) {
            let file = db.file(span.file);
            let line = file.line_col(span.start).0;
            missed.entry(span.file).or_default().push(line);
        }
        let mut files: Vec<_> = missed.into_iter().collect();
        files.sort_by_key(|(f, _)| *f);
        for (f, mut lines) in files {
            lines.sort_unstable();
            lines.dedup();
            out.push_str(&format!(
                "  {}: missed lines {}\n",
                db.file(f).name(),
                lines
                    .iter()
                    .map(|l| l.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        out
    }
}

/// Observer adapter over a shared collector, so the caller can keep a handle
/// and call [`CoverageCollector::report`] after the run.
pub struct SharedCollector(pub std::rc::Rc<std::cell::RefCell<CoverageCollector>>);

impl Observer for SharedCollector {
    fn on_event(&mut self, ev: &SimEvent) {
        self.0.borrow_mut().on_event(ev);
    }
}

impl Observer for CoverageCollector {
    fn on_event(&mut self, ev: &SimEvent) {
        match ev {
            SimEvent::StmtExec { span, .. } => {
                self.lines_hit.insert(*span);
                if self.branches_total.contains(span) {
                    self.branches_hit.insert(*span);
                }
            }
            SimEvent::SignalChange { sig, old, new, .. } => {
                let w = old.bits.len().max(new.bits.len());
                for b in 0..w {
                    let e = self.toggle.entry((*sig, b)).or_insert((false, false));
                    for v in [old.bit(b), new.bit(b)] {
                        if v == V0 {
                            e.0 = true;
                        }
                        if v == V1 {
                            e.1 = true;
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hdl_ir::{ModuleIr, ProcKind, ProcessIr, Sens};

    fn spanned(start: u32, end: u32, inner: Stmt) -> Stmt {
        Stmt::Spanned {
            span: Span::new(FileId(0), start, end),
            inner: Box::new(inner),
        }
    }

    #[test]
    fn universe_walks_if_case_and_spans() {
        // if (c) A; else B;  inside a block, plus a case with two items.
        let body = spanned(
            0,
            100,
            Stmt::Block(vec![
                spanned(
                    10,
                    50,
                    Stmt::If {
                        cond: hdl_ir::Expr::Lit(hdl_ir::Value::from_u64(1, 1)),
                        then: Box::new(spanned(12, 20, Stmt::Null)),
                        els: Some(Box::new(spanned(22, 30, Stmt::Null))),
                    },
                ),
                spanned(
                    60,
                    90,
                    Stmt::Case {
                        expr: hdl_ir::Expr::Lit(hdl_ir::Value::from_u64(1, 0)),
                        items: vec![
                            (
                                vec![hdl_ir::Expr::Lit(hdl_ir::Value::from_u64(1, 0))],
                                spanned(62, 70, Stmt::Null),
                            ),
                            (
                                vec![hdl_ir::Expr::Lit(hdl_ir::Value::from_u64(1, 1))],
                                spanned(72, 80, Stmt::Null),
                            ),
                        ],
                        default: Some(Box::new(spanned(82, 88, Stmt::Null))),
                    },
                ),
            ]),
        );
        let design = Design {
            top: None,
            modules: vec![ModuleIr {
                name: "m".into(),
                params: vec![],
                ports: vec![],
                signals: vec![],
                assigns: vec![],
                processes: vec![ProcessIr {
                    kind: ProcKind::Always,
                    sens: Sens::All,
                    body,
                    origin: "always".into(),
                }],
                functions: vec![],
                instances: vec![],
                time_unit_fs: 1_000_000,
            }],
        };
        let (lines, branches) = coverage_universe(&design);
        assert_eq!(lines.len(), 8, "all spanned stmts: {:?}", lines);
        assert_eq!(
            branches.len(),
            5,
            "then, else, 2 items, default: {:?}",
            branches
        );

        // Simulate execution of the then-arm and one case item.
        let mut cov = CoverageCollector::new(&design, &[1, 8]);
        cov.on_event(&SimEvent::StmtExec {
            span: Span::new(FileId(0), 12, 20),
            time_fs: 0,
        });
        cov.on_event(&SimEvent::StmtExec {
            span: Span::new(FileId(0), 62, 70),
            time_fs: 0,
        });
        cov.on_event(&SimEvent::SignalChange {
            sig: 1,
            old: hdl_ir::Value::from_u64(8, 0x00),
            new: hdl_ir::Value::from_u64(8, 0xff),
            time_fs: 0,
        });
        assert_eq!(cov.line_coverage(), (2, 8));
        assert_eq!(cov.branch_coverage(), (2, 5));
        assert_eq!(cov.toggle_coverage(), (8, 9)); // sig0 untouched, sig1 all bits toggled
        let mut db = SourceDb::new();
        db.add_file("t.v", "x".repeat(100)).unwrap();
        let rep = cov.report(&db);
        assert!(rep.contains("line 2/8"), "{}", rep);
        assert!(rep.contains("t.v: missed lines"), "{}", rep);
    }
}
