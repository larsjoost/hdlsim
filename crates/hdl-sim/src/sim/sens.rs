//! The sensitivity index and its lifecycle: `park`/`unpark` swap a parked
//! process's wake edges in and out of the signal → (proc, edge) index, and
//! `schedule_sensitive` wakes the fanout of a changed signal (evaluating SV
//! `iff` qualifiers at the edge). Child module of `sim` so it can read the
//! kernel's private fields, the same pattern as `sim/ckpt.rs`.

use super::{truthy, Simulator};
use hdl_ir::elab::FlatSigId;
use hdl_ir::ir::{Edge, EdgeKind, Expr};
use hdl_ir::value::{V0, V1};

impl Simulator {
    /// Park a process on an event list, at most once per process. The
    /// process's `sens_index` entries swap from its declared sensitivity to
    /// the wait's edges — while parked, only those can wake it.
    pub(super) fn park(&mut self, pid: u32, edges: Vec<Edge>) {
        let i = pid as usize;
        if self.parked[i] {
            self.trace(format!(
                "{}: park SKIPPED, already parked",
                self.flat.procs[pid as usize].origin
            ));
            return;
        }
        self.trace(format!(
            "{}: park on {:?}",
            self.flat.procs[pid as usize].origin, edges
        ));
        let old = std::mem::replace(&mut self.cur_sens[i], edges.clone());
        for e in &old {
            self.sens_index[e.sig as usize].retain(|(q, _)| *q != pid);
        }
        for e in &edges {
            self.sens_index[e.sig as usize].push((pid, e.clone()));
        }
        self.parked[i] = true;
    }

    /// Release a process from its wait, swapping its `sens_index` entries back
    /// to the declared sensitivity. Called before re-activation.
    pub(super) fn unpark(&mut self, pid: u32) {
        let i = pid as usize;
        if !self.parked[i] {
            return;
        }
        self.parked[i] = false;
        let old = std::mem::take(&mut self.cur_sens[i]);
        for e in &old {
            self.sens_index[e.sig as usize].retain(|(q, _)| *q != pid);
        }
        for e in &self.proc_sens[i] {
            self.sens_index[e.sig as usize].push((pid, e.clone()));
        }
        self.cur_sens[i] = self.proc_sens[i].clone();
    }

    /// A signal changed: wake the processes in its fanout whose edge kind
    /// matches the transition and whose `iff` qualifier (if any) is true at
    /// the edge.
    pub(super) fn schedule_sensitive(
        &mut self,
        sig: FlatSigId,
        old: &super::Value,
        new: &super::Value,
    ) {
        // Lowering-internal signals (`__for_i` loop variables, `__pN_var_*`
        // variable mirrors) are not VHDL signals: writing them must never
        // wake a process. A (ALL) process's collect_sigs sensitivity
        // includes them — the loop reads its own variable, the body reads
        // its variables — so an unfiltered wake lets a process wake on its
        // own loop/variable writes: an infinite zero-delay loop (observed
        // as a (ALL) tb stimulus process re-running on every `__for_i`
        // iteration write). Flat names are instance-qualified
        // (`inst.__p0_var_x`), so test the last path segment. VHDL
        // identifiers cannot begin with `__`, so the prefix is
        // unambiguous.
        if self.flat.signals[sig as usize]
            .name
            .rsplit('.')
            .next()
            .is_some_and(|n| n.starts_with("__"))
        {
            return;
        }
        let edge_of = |ek: &EdgeKind| match ek {
            EdgeKind::Pos => new.bit(0) == V1 && old.bit(0) != V1,
            EdgeKind::Neg => new.bit(0) == V0 && old.bit(0) != V0,
            EdgeKind::Any => true,
        };
        // Fanout-driven wake: only processes whose CURRENT sensitivity (declared
        // list, or the wait they are parked on) includes `sig` are considered —
        // O(fanout) instead of scanning every process per write. Clone the
        // (small) fanout list so the `iff` gate can evaluate through &self,
        // then wake.
        let cands = self.sens_index[sig as usize].clone();
        let mut to_wake: Vec<u32> = Vec::new();
        for (pid, e) in &cands {
            if edge_of(&e.kind) && self.edge_iff_true(&e.iff) {
                to_wake.push(*pid);
            }
        }
        for pid in to_wake {
            if self.parked[pid as usize] {
                self.trace(format!(
                    "{}: woken by edge on signal {sig} ({})",
                    self.flat.procs[pid as usize].origin, self.flat.signals[sig as usize].name
                ));
            }
            self.unpark(pid);
            self.activate(pid);
        }
    }

    /// The `iff` gate of a wake edge (SV LRM 9.4.2.3): the edge fires only
    /// when the qualifier condition is true at the edge. `None` (plain edge,
    /// or any VHDL edge) is unconditional; an x/z condition reads false.
    fn edge_iff_true(&self, iff: &Option<Expr>) -> bool {
        match iff {
            None => true,
            Some(c) => {
                // `schedule_sensitive` runs inside the fixpoint's memo region
                // (assign drives reach it through `set_sig`), but the Edge
                // trees here are per-wake/park clones — `schedule_sensitive`
                // clones the fanout list, `park`/`unpark` swap wait edges in
                // and out — so their addresses churn and get recycled, and a
                // pointer-keyed flags memo could return a stale
                // classification. Walk uncached, like the PCA trees (see
                // `sim/type_flags.rs`).
                let saved = self.expr_memo.replace(false);
                let r = truthy(&self.eval_expr(c, None));
                self.expr_memo.set(saved);
                r
            }
        }
    }
}
