//! Process-local variable lowering: each variable becomes a hidden signal
//! (VHDL variables keep their value between activations, exactly like a
//! signal), and its plain-name binding is scoped to the process.

use super::*;

impl<'a> Lower<'a> {
    /// A process-local variable becomes a (hidden) signal — VHDL variables
    /// keep their value between activations, exactly like a signal. The
    /// plain-name binding is recorded in `bound` so the caller can restore
    /// it after the process body: a variable's scope is its process (LRM
    /// 1076, process declarative part), and a leaked binding would silently
    /// redirect a later process's reference to an architecture-level object
    /// of the same name into this process's private slot.
    pub(super) fn lower_process_var(
        &mut self,
        module: &mut ModuleIr,
        d: &SyntaxNode,
        proc_idx: usize,
        bound: &mut Vec<(String, Option<SigId>)>,
    ) {
        let kids = real_children(d);
        let colon = kids
            .iter()
            .position(|k| k.kind() == SyntaxKind::Punct && self.text(k) == ":");
        let Some(colon) = colon else { return };
        let names: Vec<String> = kids[..colon]
            .iter()
            .filter(|k| k.kind() == SyntaxKind::Name)
            .map(|k| self.text(k))
            .collect();
        let type_mark = kids[colon + 1..]
            .iter()
            .find(|k| k.kind() == SyntaxKind::Name);
        let constraint = kids[colon + 1..]
            .iter()
            .find(|k| k.kind() == SyntaxKind::Range);
        // The initializer follows `:=` and may be a bare name (constant or
        // parameterless call) — see `lower_signal_decl`.
        let init = kids[colon + 1..]
            .iter()
            .position(|k| k.kind() == SyntaxKind::Punct && self.text(k) == "=")
            .and_then(|e| {
                kids[colon + 1 + e + 1..]
                    .iter()
                    .find(|k| is_expr_node(k.kind()))
                    .cloned()
            });
        let span = d.span();
        let type_name = type_mark.map(|tm| self.text(tm).to_ascii_lowercase());
        let (width, signed, _, real) = match type_mark {
            Some(tm) => self.resolve_type(tm, constraint, span),
            None => (32, false, None, false),
        };
        let init_v = init.and_then(|e| {
            // A string target's `(others => ' ')` replicate must cover the
            // whole object; without the target context it baked a fixed
            // 32-bit count (see `agg_target_w`).
            let saved_w = self.agg_target_w;
            if e.kind() == SyntaxKind::Aggregate
                && type_mark.is_some_and(|tm| self.string_typed(tm))
            {
                self.agg_target_w = Some(width);
            }
            let ex = self.lower_expr(&e, &self.consts.clone());
            self.agg_target_w = saved_w;
            self.ce(&ex)
        });
        for n in &names {
            let id = module.signals.len() as SigId;
            module.signals.push(SignalIr {
                name: format!("__p{}_var_{}", proc_idx, n),
                width,
                kind: if real { SigKind::Real } else { SigKind::Wire },
                init: init_v.clone(),
                mem: None,
                signed,
            });
            self.sig_signed.insert(id, signed);
            self.sig_width.insert(id, width);
            self.record_obj_shape(id, type_mark, constraint);
            if let Some(tn) = &type_name {
                if let Some(rn) = self.as_record(tn) {
                    self.sig_rec.insert(id, rn.clone());
                    module
                        .real_ranges
                        .insert(n.clone(), self.record_real_ranges(&rn));
                }
                if let Some(en) = self.as_elem_record(tn) {
                    self.sig_elem_rec.insert(id, en);
                }
                // Protected-type object local: member selects placeholder.
                if matches!(self.types.get(tn), Some(TypeInfo::Handle)) {
                    self.sig_prot.insert(id, tn.clone());
                }
                if let Some(TypeInfo::Array { dims, .. }) = self.types.get(tn) {
                    if !dims.is_empty() {
                        let dd = match constraint {
                            Some(c) => self.constraint_dims(c),
                            None => dims.clone(),
                        };
                        self.sig_dims.insert(id, dd);
                    }
                }
            }
            bound.push((n.clone(), self.names.insert(n.clone(), id)));
        }
    }
}
