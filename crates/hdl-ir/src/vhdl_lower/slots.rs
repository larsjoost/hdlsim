//! Subprogram slot-space scope: the per-call slot-id machinery.
//!
//! Slot ids alias the flat-signal id space — module signals can hold the
//! same ids — so a subprogram's entries must not leak into the enclosing
//! scope after its body lowers. This module installs a subprogram's slot
//! space (`install_fn_slots`) and owns the saved-state guards restored at
//! exit: the per-slot shape/record snapshot (`SlotShapeSave`) and the
//! enclosing `fn_formals` set (`FnSlotScope`), plus the per-call mutable
//! collections threaded through declarative-item and body lowering
//! (`FnLocalDecls`).
//!
//! Extracted from subprogram.rs (pure move, no logic changes).

use super::*;

/// Per-call mutable collections threaded through a subprogram's
/// declarative-item and body lowering: the slot list (name, width), the
/// lowered locals, their declared type marks, and the two per-call queues
/// (dynamic-width locals re-folded by const-eval; constant initializers
/// prepended to the body).
pub(super) struct FnLocalDecls {
    pub(super) slots: Vec<(String, usize)>,
    pub(super) locals: Vec<SignalIr>,
    pub(super) local_types: Vec<(String, String)>,
    pub(super) dyn_locals: Vec<(SigId, Expr)>,
    pub(super) pending_const_inits: Vec<(SigId, Expr)>,
}

/// Saved outer-scope state from installing a subprogram's slot space:
/// the per-slot shape/record maps and the enclosing `fn_formals` set.
/// Both are restored together at exit — the shapes restore must run after
/// the return-width fixups, which still read the function's own slot
/// mappings.
pub(super) struct FnSlotScope {
    saved_shapes: SlotShapeSave,
    saved_formals: std::collections::HashSet<SigId>,
}

impl FnSlotScope {
    pub(super) fn restore(self, lx: &mut Lower<'_>) {
        self.saved_shapes.restore(lx);
        lx.fn_formals = self.saved_formals;
    }
}

/// Entry-state snapshot of the per-slot shape and record maps for the slot
/// ids a subprogram is about to register. Captured at entry, dropped for
/// the new slots, restored exactly at exit. The record maps
/// (`rec`/`elem_rec`/`prot`) are captured here too: nothing between the
/// capture point and their original later capture site mutates them, so one
/// snapshot is behavior-identical to the previous split capture.
struct SlotShapeSave {
    len: usize,
    elem: HashMap<SigId, Option<usize>>,
    asc: HashMap<SigId, Option<(i64, bool)>>,
    dynw: HashMap<SigId, Option<Expr>>,
    dims: HashMap<SigId, Option<Vec<(i64, usize)>>>,
    rec: HashMap<SigId, Option<String>>,
    elem_rec: HashMap<SigId, Option<String>>,
    prot: HashMap<SigId, Option<String>>,
}

fn capture_map<T: Clone>(map: &HashMap<SigId, T>, len: usize) -> HashMap<SigId, Option<T>> {
    (0..len)
        .map(|i| {
            let id = i as SigId;
            (id, map.get(&id).cloned())
        })
        .collect()
}

fn restore_map<T: Clone>(map: &mut HashMap<SigId, T>, saved: HashMap<SigId, Option<T>>) {
    for (id, v) in saved {
        match v {
            Some(v) => {
                map.insert(id, v);
            }
            None => {
                map.remove(&id);
            }
        }
    }
}

impl SlotShapeSave {
    fn capture(lx: &Lower<'_>, len: usize) -> Self {
        Self {
            len,
            elem: capture_map(&lx.sig_elem, len),
            asc: capture_map(&lx.sig_asc, len),
            dynw: capture_map(&lx.sig_dynw, len),
            dims: capture_map(&lx.sig_dims, len),
            rec: capture_map(&lx.sig_rec, len),
            elem_rec: capture_map(&lx.sig_elem_rec, len),
            prot: capture_map(&lx.sig_prot, len),
        }
    }

    /// Drop the shape entries under the subprogram's slot ids so a stale
    /// entry from a previously lowered subprogram cannot mis-address this
    /// one's slices or fold its attributes from another function's object.
    /// `record_obj_shape` re-adds what this subprogram's objects have.
    fn drop_entries(&self, lx: &mut Lower<'_>) {
        for i in 0..self.len {
            let id = i as SigId;
            lx.sig_asc.remove(&id);
            lx.sig_elem.remove(&id);
            lx.sig_dynw.remove(&id);
            lx.sig_dims.remove(&id);
        }
    }

    fn restore(self, lx: &mut Lower<'_>) {
        restore_map(&mut lx.sig_elem, self.elem);
        restore_map(&mut lx.sig_asc, self.asc);
        restore_map(&mut lx.sig_dynw, self.dynw);
        restore_map(&mut lx.sig_dims, self.dims);
        restore_map(&mut lx.sig_rec, self.rec);
        restore_map(&mut lx.sig_elem_rec, self.elem_rec);
        restore_map(&mut lx.sig_prot, self.prot);
    }
}

impl<'a> Lower<'a> {
    /// Install a subprogram's slot space: push the enclosing `names` map
    /// onto `names_stack`, build the slots (slot 0 = return value, then the
    /// formals — installed first so function-local constants can reference
    /// parameters: `constant pos : time := ifelse(T >= 0 ns, T, -T);`),
    /// mark the formals in `fn_formals` (they are sized by the call's
    /// actuals), drop stale per-slot shape entries from a previously lowered
    /// subprogram (slot ids alias across subprograms), and record this
    /// subprogram's object shapes and record/protected mappings. Returns
    /// the slot list and the saved outer state to restore at exit.
    pub(super) fn install_fn_slots(
        &mut self,
        name: &str,
        ret_width: usize,
        args: &[(String, usize, String)],
        arg_types: &[(String, String)],
        arg_shapes: &[(String, Option<SyntaxNode>, Option<SyntaxNode>)],
    ) -> (Vec<(String, usize)>, FnSlotScope) {
        let saved_names = std::mem::take(&mut self.names);
        self.names_stack.push(saved_names);
        let mut slots: Vec<(String, usize)> = vec![(name.to_string(), ret_width.max(1))];
        slots.extend(args.iter().map(|(n, w, _)| (n.clone(), *w)));
        self.names = slots
            .iter()
            .enumerate()
            .map(|(i, (n, _))| (n.clone(), i as SigId))
            .collect();
        let saved_formals = std::mem::take(&mut self.fn_formals);
        self.fn_formals = (1..slots.len()).map(|i| i as SigId).collect();
        // Capture the pre-call shape/record state and clear the shape
        // entries under this subprogram's slot ids — a stale entry would
        // mis-address this one's slices and fold its attributes from another
        // function's object. `record_obj_shape` re-adds what this
        // subprogram's objects have; the exit restore is the guard's.
        let saved_shapes = SlotShapeSave::capture(self, slots.len());
        saved_shapes.drop_entries(self);
        for (n, tmark, constraint) in arg_shapes {
            if let Some(&id) = self.names.get(n) {
                self.record_obj_shape(id, tmark.as_ref(), constraint.as_ref());
            }
        }
        // Record-typed parameters: remember the record type so `arg.field`
        // resolves even in declarative-item initializers (constants), which
        // are lowered after this. Slot ids are per-function, so entries left
        // by a previously lowered function must be dropped first — a formal
        // whose type is not a record/array-of-records would otherwise
        // inherit the stale record of an earlier function's same-numbered
        // slot. The entry-state capture and exit reset are the guard's.
        self.map_slot_records(&slots, arg_types);
        let guard = FnSlotScope {
            saved_shapes,
            saved_formals,
        };
        (slots, guard)
    }
}
