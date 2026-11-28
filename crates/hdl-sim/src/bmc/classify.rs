//! Signal classification for BMC: which flat signals are state,
//! combinational, or free inputs, and the topological order of the
//! combinational writers. Split out of `bmc.rs`.

use super::stmt_scan::{lhs_roots, stmt_has_sva, stmt_writes};
use hdl_ir::elab::{Flat, FlatSigId};
use hdl_ir::{Expr, ProcKind, Sens, SigKind};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum ProcCls {
    Init,
    Comb,
    Clocked,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Writer {
    Assign(usize),
    Proc(usize),
}

pub(super) struct Class {
    pub(super) state: Vec<FlatSigId>,
    pub(super) input: Vec<FlatSigId>,
    /// Comb writer (index into comb_order) of each comb signal.
    pub(super) comb_writer_of: HashMap<FlatSigId, usize>,
    /// Comb writers (assigns + @* procs) in topological order.
    pub(super) comb_order: Vec<Writer>,
    /// Per flat proc: class + written signals.
    pub(super) procs: Vec<(ProcCls, Vec<FlatSigId>)>,
    /// Indices of processes containing assert syscalls (property sources).
    pub(super) sva_procs: Vec<usize>,
}

impl Class {
    pub(super) fn is_comb(&self, s: FlatSigId) -> bool {
        self.comb_writer_of.contains_key(&s)
    }
}

pub(super) fn classify(flat: &Flat) -> Result<Class, String> {
    error_on_memories(flat)?;
    // Per-process classification and writes. `has_sva` is tracked separately:
    // a process may mix assignments and asserts (VHDL style), in which case it
    // both drives logic and contributes properties.
    let mut drv = DriverSets::default();
    let (procs, sva_procs) = classify_procs(flat, &mut drv)?;
    classify_assigns(flat, &mut drv)?;
    check_write_conflicts(flat, &drv)?;
    check_driver_counts(flat, &drv)?;
    let (state, _comb, input) = partition_signals(flat, &drv);
    let (comb_writer_of, comb_order) = comb_topo_order(flat, &procs)?;

    Ok(Class {
        state,
        input,
        comb_writer_of,
        comb_order,
        procs,
        sva_procs,
    })
}

/// Per-signal driver census collected while classifying writers.
#[derive(Default)]
struct DriverSets {
    state_writers: HashSet<FlatSigId>,
    comb_writers: HashSet<FlatSigId>,
    init_writers: HashSet<FlatSigId>,
    clocked_count: HashMap<FlatSigId, usize>,
    comb_count: HashMap<FlatSigId, usize>,
}

/// Memories are unsupported: error if referenced anywhere.
fn error_on_memories(flat: &Flat) -> Result<(), String> {
    let mut referenced: Vec<FlatSigId> = Vec::new();
    for a in &flat.assigns {
        a.lhs.collect_sigs(&mut referenced);
        a.rhs.collect_sigs(&mut referenced);
    }
    for p in &flat.procs {
        p.body.collect_sigs(&mut referenced);
        let mut w = Vec::new();
        stmt_writes(&p.body, &mut w);
        referenced.extend(w);
    }
    for &s in &referenced {
        if flat.signals[s as usize].mem.is_some() {
            return Err(format!(
                "prove: memories are not supported yet (signal '{}')",
                flat.signals[s as usize].name
            ));
        }
    }
    Ok(())
}

/// Per-process class + the (deduplicated) signals each process writes.
type ProcWrites = Vec<(ProcCls, Vec<FlatSigId>)>;

/// Classify each process and record the signals it writes.
fn classify_procs(flat: &Flat, drv: &mut DriverSets) -> Result<(ProcWrites, Vec<usize>), String> {
    let mut procs: Vec<(ProcCls, Vec<FlatSigId>)> = Vec::new();
    let mut sva_procs: Vec<usize> = Vec::new();
    for p in &flat.procs {
        let mut writes = Vec::new();
        stmt_writes(&p.body, &mut writes);
        writes.sort_unstable();
        writes.dedup();
        let cls = match (p.kind, &p.sens) {
            (ProcKind::Initial, _) => ProcCls::Init,
            (_, Sens::All) => ProcCls::Comb,
            (_, Sens::List(_)) => ProcCls::Clocked,
        };
        for &s in &writes {
            match cls {
                ProcCls::Clocked => {
                    *drv.clocked_count.entry(s).or_insert(0) += 1;
                    drv.state_writers.insert(s);
                }
                ProcCls::Comb => {
                    *drv.comb_count.entry(s).or_insert(0) += 1;
                    drv.comb_writers.insert(s);
                }
                ProcCls::Init => {
                    drv.init_writers.insert(s);
                }
            }
        }
        if stmt_has_sva(&p.body) {
            sva_procs.push(procs.len());
        }
        procs.push((cls, writes));
    }
    Ok((procs, sva_procs))
}

/// Continuous assigns are comb writers.
fn classify_assigns(flat: &Flat, drv: &mut DriverSets) -> Result<(), String> {
    for a in &flat.assigns {
        if !matches!(a.lhs, Expr::Sig(_)) {
            return Err(
                "prove: only plain-signal LHS is supported in continuous assigns".to_string(),
            );
        }
        let mut roots = Vec::new();
        lhs_roots(&a.lhs, &mut roots);
        for s in roots {
            *drv.comb_count.entry(s).or_insert(0) += 1;
            drv.comb_writers.insert(s);
        }
    }
    Ok(())
}

/// Driver conflicts: sequential vs combinational is an error; an initial
/// block may additionally set the step-0 value of a clocked signal.
fn check_write_conflicts(flat: &Flat, drv: &DriverSets) -> Result<(), String> {
    for &s in &drv.state_writers {
        if drv.comb_writers.contains(&s) {
            return Err(format!(
                "prove: signal '{}' is driven by both sequential and combinational logic",
                flat.signals[s as usize].name
            ));
        }
    }
    for &s in &drv.comb_writers {
        if drv.init_writers.contains(&s) {
            return Err(format!(
                "prove: signal '{}' is driven by both an initial block and combinational logic",
                flat.signals[s as usize].name
            ));
        }
    }
    Ok(())
}

/// A signal has at most one clocked and at most one comb driver; top input
/// ports must not be driven internally.
fn check_driver_counts(flat: &Flat, drv: &DriverSets) -> Result<(), String> {
    for (&s, &n) in &drv.clocked_count {
        if n > 1 {
            return Err(format!(
                "prove: signal '{}' has {} clocked drivers (races are not modeled)",
                flat.signals[s as usize].name, n
            ));
        }
    }
    for (&s, &n) in &drv.comb_count {
        if n > 1 {
            return Err(format!(
                "prove: signal '{}' has {} combinational drivers",
                flat.signals[s as usize].name, n
            ));
        }
    }
    for (i, sig) in flat.signals.iter().enumerate() {
        if sig.scope == flat.top_scope
            && matches!(sig.kind, SigKind::Input | SigKind::Inout)
            && (drv.state_writers.contains(&(i as FlatSigId))
                || drv.comb_writers.contains(&(i as FlatSigId))
                || drv.init_writers.contains(&(i as FlatSigId)))
        {
            return Err(format!(
                "prove: top-level input '{}' is driven inside the design",
                sig.name
            ));
        }
    }
    Ok(())
}

/// Partition all signals into state, combinational, and free inputs.
fn partition_signals(
    flat: &Flat,
    drv: &DriverSets,
) -> (Vec<FlatSigId>, Vec<FlatSigId>, Vec<FlatSigId>) {
    let mut state: Vec<FlatSigId> = drv.state_writers.iter().copied().collect();
    state.extend(drv.init_writers.iter().copied());
    // Signals with a declaration init but no driver are constant state.
    for (i, sig) in flat.signals.iter().enumerate() {
        let id = i as FlatSigId;
        if sig.init.is_some() && !drv.comb_writers.contains(&id) && !state.contains(&id) {
            state.push(id);
        }
    }
    state.sort_unstable();
    state.dedup();

    let mut comb: Vec<FlatSigId> = drv.comb_writers.iter().copied().collect();
    comb.sort_unstable();

    // Everything not state or comb is a free input (top ports + undriven wires).
    let mut input: Vec<FlatSigId> = Vec::new();
    for i in 0..flat.signals.len() {
        let id = i as FlatSigId;
        if !state.contains(&id) && !comb.contains(&id) {
            input.push(id);
        }
    }
    (state, comb, input)
}

/// Comb writer graph + topological order (cycle = error).
fn comb_topo_order(
    flat: &Flat,
    procs: &[(ProcCls, Vec<FlatSigId>)],
) -> Result<(HashMap<FlatSigId, usize>, Vec<Writer>), String> {
    let writers = collect_writers(flat, procs);
    let comb_writer_of = writer_roots(flat, &writers, procs);
    let deps = writer_deps(flat, &writers, &comb_writer_of)?;
    let order = kahn_order(&deps, writers.len())?;
    let comb_order: Vec<Writer> = order.into_iter().map(|i| writers[i]).collect();
    Ok((comb_writer_of, comb_order))
}

/// All comb writers in flat order: continuous assigns, then @* processes.
fn collect_writers(flat: &Flat, procs: &[(ProcCls, Vec<FlatSigId>)]) -> Vec<Writer> {
    let mut writers: Vec<Writer> = Vec::new();
    for (ai, _) in flat.assigns.iter().enumerate() {
        writers.push(Writer::Assign(ai));
    }
    for (pi, (cls, _)) in procs.iter().enumerate() {
        if *cls == ProcCls::Comb {
            writers.push(Writer::Proc(pi));
        }
    }
    writers
}

/// Map each comb signal to its writer index.
fn writer_roots(
    flat: &Flat,
    writers: &[Writer],
    procs: &[(ProcCls, Vec<FlatSigId>)],
) -> HashMap<FlatSigId, usize> {
    let mut comb_writer_of: HashMap<FlatSigId, usize> = HashMap::new();
    for (wi, w) in writers.iter().enumerate() {
        let roots = match w {
            Writer::Assign(ai) => {
                let mut r = Vec::new();
                lhs_roots(&flat.assigns[*ai].lhs, &mut r);
                r
            }
            Writer::Proc(pi) => procs[*pi].1.clone(),
        };
        for r in roots {
            comb_writer_of.insert(r, wi);
        }
    }
    comb_writer_of
}

/// deps[wi] = writers wi depends on (reads of comb signals).
fn writer_deps(
    flat: &Flat,
    writers: &[Writer],
    comb_writer_of: &HashMap<FlatSigId, usize>,
) -> Result<Vec<Vec<usize>>, String> {
    let mut deps: Vec<Vec<usize>> = vec![Vec::new(); writers.len()];
    for (wi, w) in writers.iter().enumerate() {
        let mut reads = Vec::new();
        match w {
            Writer::Assign(ai) => flat.assigns[*ai].rhs.collect_sigs(&mut reads),
            Writer::Proc(pi) => flat.procs[*pi].body.collect_sigs(&mut reads),
        }
        for r in reads {
            match comb_writer_of.get(&r) {
                Some(&dj) if dj != wi => deps[wi].push(dj),
                Some(_) => {
                    return Err(format!(
                        "prove: combinational cycle through '{}'",
                        flat.signals[r as usize].name
                    ));
                }
                None => {}
            }
        }
    }
    Ok(deps)
}

/// Kahn topological sort (dependencies first); partial order = cycle.
fn kahn_order(deps: &[Vec<usize>], n: usize) -> Result<Vec<usize>, String> {
    let mut indeg = vec![0usize; n];
    let mut succ: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (wi, d) in deps.iter().enumerate() {
        for &dj in d {
            succ[dj].push(wi);
            indeg[wi] += 1;
        }
    }
    let mut queue: Vec<usize> = (0..n).filter(|&i| indeg[i] == 0).collect();
    let mut order = Vec::new();
    while let Some(wi) = queue.pop() {
        order.push(wi);
        for &nxt in &succ[wi] {
            indeg[nxt] -= 1;
            if indeg[nxt] == 0 {
                queue.push(nxt);
            }
        }
    }
    if order.len() != n {
        return Err("prove: combinational cycle detected".to_string());
    }
    Ok(order)
}
