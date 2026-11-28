//! Assignment execution: writing a value to an assign target — a whole
//! signal, a memory word, a bit, or a slice — plus the continuous-assign
//! fixpoint sweep's target machinery and the memory-grow RSS guard.
//!
//! Child module of `sim` so it can read the kernel's private fields, the
//! same pattern as `sim/probe.rs`.

use super::Simulator;
use crate::events::{MessageSeverity, SimEvent};
use hdl_ir::elab::FlatSigId;
use hdl_ir::{BinOp, Expr, UnOp, Value};

/// Which operands of a binary op are context-determined (re-extended to the
/// assignment context width) vs self-determined (LRM 11.6, Table 11-21):
/// shifts/rotates take the left operand's width, so the amount is
/// self-determined; arithmetic and bitwise ops are context-determined on
/// both sides; comparisons and logical ops are self-determined (1-bit
/// result).
fn ctx_operand_arms(op: BinOp) -> (bool, bool) {
    if matches!(
        op,
        BinOp::Shl | BinOp::Shr | BinOp::Sar | BinOp::Rol | BinOp::Ror
    ) {
        (true, false)
    } else if matches!(
        op,
        BinOp::Add
            | BinOp::Sub
            | BinOp::Mul
            | BinOp::Div
            | BinOp::Mod
            | BinOp::Pow
            | BinOp::BAnd
            | BinOp::BOr
            | BinOp::BXor
            | BinOp::BNand
            | BinOp::BNor
            | BinOp::BXnor
    ) {
        (true, true)
    } else {
        (false, false)
    }
}

impl Simulator {
    /// Wake processes sensitive to a memory (array) signal after an element
    /// write. Memory words live in `mems`, not `vals`, so element writes
    /// bypass `set_sig` — but a VHDL event on any element is still an event
    /// on the signal (LRM 1076.4 sensitivity sets use the longest static
    /// prefix), so a process parked on the array must wake. Only a value
    /// change wakes, matching the scalar path.
    fn wake_mem_watchers(&mut self, sig: usize, a: usize, old_elem: Value) {
        let new_elem = self.mems[sig][a].clone();
        if old_elem != new_elem {
            self.schedule_sensitive(sig as FlatSigId, &old_elem, &new_elem);
        }
    }

    /// Whole-signal update from every path that assigns a bare signal id
    /// (continuous assigns, NBA region, time-wheel events): memory-backed
    /// signals distribute across their words, plain signals take the flat
    /// value. All whole-array semantics live here so the paths cannot
    /// diverge.
    pub(super) fn apply_whole_assign(&mut self, sig: FlatSigId, v: Value) -> bool {
        if self.is_mem[sig as usize] {
            self.mem_distribute(sig as usize, v)
        } else {
            let w = self.flat.signals[sig as usize].width.max(1);
            self.set_sig(sig, v.resize(w))
        }
    }

    /// Whole-array assign onto a memory-backed signal: distribute the flat
    /// value across the words (element 0 takes the most significant part),
    /// then mirror the flat value into `vals` so watch/wave output and
    /// scalar sensitivity see the same value a whole-signal read returns.
    fn mem_distribute(&mut self, sig: usize, v: Value) -> bool {
        let w = self.flat.signals[sig].width.max(1);
        // First write materializes the words at the declared depth — an
        // empty `mems` here would drop the write and the array would stay
        // empty (every later element read X) forever.
        if self.mems[sig].is_empty() {
            let depth = self.flat.signals[sig].mem.unwrap_or(0);
            if depth == 0 {
                return false;
            }
            self.mems[sig].resize(depth, Value::x(w));
        }
        let n = self.mems[sig].len();
        let total = w * n;
        let flat = v.resize(total);
        for i in 0..n {
            let el = flat.slice(total - 1 - i * w, total - (i + 1) * w);
            if self.mems[sig][i] != el {
                let old = self.mems[sig][i].clone();
                self.mems[sig][i] = el;
                self.wake_mem_watchers(sig, i, old);
            }
        }
        self.set_sig(sig as FlatSigId, flat)
    }

    // ---- assignment targets -------------------------------------------------------------

    /// Cap for growing a memory on an indexed write (`--max-signal-mb`,
    /// shared with `alloc_report`'s per-signal estimate): a write at an index
    /// in the 2³² range is an underflowed/negative address from an
    /// unelaborated generic, and `resize` would abort the process (96 GiB
    /// class) before any RSS guard could run. Over the cap: report the
    /// signal, the index, and the declared depth, and stop the run.
    pub(super) fn mem_grow_ok(&mut self, sig: usize, a: usize) -> bool {
        let cap_bytes = crate::meminfo::signal_limit_bytes();
        // Same estimate as `alloc_report`: a stored word costs its Value
        // header (~24 bytes) plus 1 byte per bit — a 1-bit memory at 2³²
        // words is ~96 GiB of headers alone, so the cap must be on bytes,
        // not word count.
        let bytes = (24 + self.flat.signals[sig].width.max(1) as u64).saturating_mul(a as u64 + 1);
        if bytes <= cap_bytes {
            return true;
        }
        // Report the first violation only: a runaway loop writes thousands of
        // out-of-range indexes within one delta, and each report is a full
        // sentence.
        if !self.error_stopped {
            self.emit(SimEvent::Message {
                scope: self.cur_scope(),
                text: format!(
                    "error: memory write to '{}' at index {} would grow it past \
                     --max-signal-mb (declared depth {} words) — the index is \
                     probably an underflowed or negative address (an unelaborated \
                     generic or range); raise --max-signal-mb only if the \
                     design is genuinely this big",
                    self.flat.signals[sig].name,
                    a,
                    self.flat.signals[sig].mem.unwrap_or(0)
                ),
                time_fs: self.time,
                span: self.cur_span,
                severity: Some(MessageSeverity::Error),
            });
        }
        self.stopped = true;
        self.error_stopped = true;
        false
    }

    /// Assign `v` to the target of `lhs`. Returns true if a signal changed.
    pub(super) fn assign_to(&mut self, lhs: &Expr, v: Value, locals: Option<&Vec<Value>>) -> bool {
        match lhs {
            Expr::Sig(s) => {
                let sig = *s as FlatSigId;
                // Whole-array assign (`arr <= arr_port`): distribute the
                // value across the memory words instead of dropping it.
                self.apply_whole_assign(sig, v)
            }
            Expr::Idx { e, i } => {
                let addr = self.eval_expr(i, locals);
                let Some(a) = addr.to_u64() else { return false };
                match &**e {
                    Expr::Sig(s) if self.is_mem[*s as usize] => {
                        let sig = *s as usize;
                        let w = self.flat.signals[sig].width.max(1);
                        // A word write must report a real change so the
                        // continuous-assign fixpoint re-sweeps: assigns are
                        // evaluated in order, and without a reported change a
                        // whole-memory compare scheduled before this write
                        // (`arr = (OTHERS => '1')`) would never re-run — the
                        // fixpoint stops on the first change-free sweep.
                        let mut changed = false;
                        if (a as usize) < self.mems[sig].len() {
                            let old = self.mems[sig][a as usize].clone();
                            let new = v.resize(w);
                            changed = old != new;
                            self.mems[sig][a as usize] = new;
                            self.wake_mem_watchers(sig, a as usize, old);
                        } else if self.mem_grow_ok(sig, a as usize) {
                            let old = Value::x(w);
                            self.mems[sig].resize(a as usize + 1, Value::x(w));
                            let new = v.resize(w);
                            changed = old != new;
                            self.mems[sig][a as usize] = new;
                            self.wake_mem_watchers(sig, a as usize, old);
                        }
                        changed
                    }
                    _ => {
                        // Bit write.
                        let sig = match &**e {
                            Expr::Sig(s) => *s as FlatSigId,
                            _ => return false,
                        };
                        let mut cur = self.vals[sig as usize].clone();
                        cur.set_bit(a as usize, v.bit(0));
                        self.set_sig(sig, cur)
                    }
                }
            }
            Expr::Sel { e, msb, lsb } => {
                let msb = self.eval_expr(msb, locals).to_u64().unwrap_or(0) as usize;
                let lsb = self.eval_expr(lsb, locals).to_u64().unwrap_or(0) as usize;
                // `arr(i).field <= v` — slice write into a memory element.
                if let Expr::Idx { e: ie, i } = &**e {
                    if let Expr::Sig(s) = &**ie {
                        let sig = *s as usize;
                        if self.is_mem[sig] {
                            let Some(a) = self.eval_expr(i, locals).to_u64() else {
                                return false;
                            };
                            let w = self.flat.signals[sig].width.max(1);
                            if (a as usize) >= self.mems[sig].len()
                                && !self.mem_grow_ok(sig, a as usize)
                            {
                                return false;
                            }
                            if (a as usize) >= self.mems[sig].len() {
                                self.mems[sig].resize(a as usize + 1, Value::x(w));
                            }
                            let old = self.mems[sig][a as usize].clone();
                            let mut cur = old.clone();
                            // A well-formed slice entirely beyond the word's
                            // width has no storage to write: drop it instead
                            // of clamping onto the top bit (same rule as the
                            // signal arm below — two clamped half-writes with
                            // different sources would fight every fixpoint
                            // sweep and trip the combinational-loop guard).
                            if msb >= lsb && lsb >= cur.bits.len() {
                                return false;
                            }
                            // Same clamp as the signal arm: dynamic slice
                            // bounds must not wrap the bit loop.
                            let msb = msb.min(cur.bits.len().saturating_sub(1));
                            let lsb = lsb.min(msb);
                            let v = v.resize(msb - lsb + 1);
                            for j in 0..=(msb - lsb) {
                                cur.set_bit(lsb + j, v.bit(j));
                            }
                            // Report the change for the same fixpoint reason
                            // as the whole-word write above.
                            let changed = old != cur;
                            self.mems[sig][a as usize] = cur;
                            self.wake_mem_watchers(sig, a as usize, old);
                            return changed;
                        }
                    }
                    return false;
                }
                let sig = match &**e {
                    Expr::Sig(s) => *s as FlatSigId,
                    _ => return false,
                };
                // Mirror the read path (`Value::slice`): clamp a reversed or
                // oversized dynamic slice instead of letting `msb - lsb`
                // wrap. A reversed dynamic bound (`sig(0 downto j)` with j>0)
                // underflowed to usize::MAX here and the bit loop spun
                // forever inside one call — unreachable by any pump-level
                // interrupt check.
                let len = self.vals[sig as usize].bits.len();
                // A well-formed slice entirely beyond the signal's width has
                // no storage to change: drop the write instead of clamping
                // it onto the top bit — two clamped half-writes with
                // different sources would otherwise fight every fixpoint
                // sweep and trip the combinational-loop guard on an acyclic
                // assign graph (a top signal left narrow by an unbound
                // generic, connected to a default-width child port).
                if msb >= lsb && lsb >= len {
                    return false;
                }
                let msb = msb.min(len.saturating_sub(1));
                let lsb = lsb.min(msb);
                let mut cur = self.vals[sig as usize].clone();
                let v = v.resize(msb - lsb + 1);
                for j in 0..=(msb - lsb) {
                    cur.set_bit(lsb + j, v.bit(j));
                }
                self.set_sig(sig, cur)
            }
            Expr::Concat(parts) => {
                // Split v across parts: parts[0] takes the most-significant
                // slice, matching Value::concat.
                let widths: Vec<usize> = parts.iter().map(|p| self.expr_width(p, locals)).collect();
                let total: usize = widths.iter().sum::<usize>().max(1);
                let mut changed = false;
                let mut hi = total; // exclusive upper bound of the remaining slice
                for (p, w) in parts.iter().zip(widths.iter()) {
                    let lo = hi.saturating_sub(*w);
                    let part_val = v.slice((hi - 1).min(total - 1), lo);
                    changed |= self.assign_to(p, part_val, locals);
                    hi = lo;
                }
                changed
            }
            _ => false,
        }
    }

    // ---- assignment-context RHS evaluation ----------------------------------------------

    /// Evaluate an assignment RHS in the assignment's width context (LRM
    /// 11.4.14 / 11.6): with a concatenation LHS, `{carry, sum} = a + b`
    /// makes `a + b` a 9-bit addition whose bit 8 is the carry — the RHS is
    /// evaluated in the context of the LHS width, not at its own
    /// self-determined width. The rule applies to concatenation targets
    /// only: VHDL shares this evaluator and sizes its operations from the
    /// operands alone (no assignment context), and a concatenation
    /// assignment target is a Verilog-only shape. Reals are skipped too:
    /// real arithmetic has no integer sizing to extend.
    pub(super) fn eval_assign_rhs(
        &self,
        lhs: &Expr,
        rhs: &Expr,
        locals: Option<&Vec<Value>>,
    ) -> Value {
        if !matches!(lhs, Expr::Concat(_)) || self.expr_real(rhs, locals) {
            return self.eval_expr(rhs, locals);
        }
        // The context width is the sum of the part widths — the same widths
        // `assign_to` splits the value by, so the slices line up.
        let ctx = self.expr_width(lhs, locals);
        if ctx <= self.expr_width(rhs, locals) {
            // Nothing to widen: at or below the RHS's own self-determined
            // width the evaluation is unchanged.
            return self.eval_expr(rhs, locals);
        }
        let widened = self.widen_ctx(rhs, ctx, locals, false);
        self.eval_expr(&widened, locals)
    }

    /// Widen a context-determined RHS tree to the assignment context width
    /// (LRM 11.6, Table 11-21): context-determined operators re-extend their
    /// operands to the context width so the operation itself computes at
    /// that width (an add's carry survives into the wide LHS), while leaves
    /// and self-determined sub-expressions keep their value and only get a
    /// value-preserving extension. `sign_ext` is set inside a `Signed`
    /// marker: its leaves must sign-extend (a zero-extend under `signed'()`
    /// would flip a negative operand's bits).
    fn widen_ctx(&self, e: &Expr, ctx: usize, locals: Option<&Vec<Value>>, sign_ext: bool) -> Expr {
        match e {
            Expr::Bin { op, l, r } => {
                let (lw, rw) = ctx_operand_arms(*op);
                Expr::Bin {
                    op: *op,
                    l: Box::new(if lw {
                        self.widen_ctx(l, ctx, locals, sign_ext)
                    } else {
                        self.keep_ctx(l, ctx, locals, sign_ext)
                    }),
                    r: Box::new(if rw {
                        self.widen_ctx(r, ctx, locals, sign_ext)
                    } else {
                        self.keep_ctx(r, ctx, locals, sign_ext)
                    }),
                }
            }
            Expr::Un { op, e } => {
                // Reductions are self-determined (1-bit result); every other
                // unary op inherits the context width through its operand.
                let inner = if matches!(
                    op,
                    UnOp::RedAnd
                        | UnOp::RedOr
                        | UnOp::RedXor
                        | UnOp::RedNand
                        | UnOp::RedNor
                        | UnOp::RedXnor
                ) {
                    self.keep_ctx(e, ctx, locals, sign_ext)
                } else {
                    self.widen_ctx(e, ctx, locals, sign_ext)
                };
                Expr::Un {
                    op: *op,
                    e: Box::new(inner),
                }
            }
            Expr::Cond { c, t, e } => Expr::Cond {
                c: Box::new(self.keep_ctx(c, ctx, locals, sign_ext)),
                t: Box::new(self.widen_ctx(t, ctx, locals, sign_ext)),
                e: Box::new(self.widen_ctx(e, ctx, locals, sign_ext)),
            },
            // The signedness marker is transparent: widen inside it, with
            // leaves sign-extending.
            Expr::Signed(inner) => Expr::Signed(Box::new(self.widen_ctx(inner, ctx, locals, true))),
            other => self.keep_ctx(other, ctx, locals, sign_ext),
        }
    }

    /// Keep a sub-expression's own value in the widened tree: extend it to
    /// the context width only when narrower, sign- or zero-extending per its
    /// own signedness (or the enclosing `signed'()` marker) — both
    /// value-preserving.
    fn keep_ctx(&self, e: &Expr, ctx: usize, locals: Option<&Vec<Value>>, sign_ext: bool) -> Expr {
        if self.expr_real(e, locals) || self.expr_width(e, locals) >= ctx {
            return e.clone();
        }
        Expr::Resize {
            e: Box::new(e.clone()),
            width: ctx,
            signed: sign_ext || self.expr_signed(e, locals),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hdl_ir::elab::{Flat, FlatSig, Scope, ScopeId};
    use hdl_ir::ir::SigKind;
    use hdl_ir::BinOp;

    /// Signals: 0/1 = 8-bit operands, 2 = 1-bit carry, 3 = 8-bit sum,
    /// 4 = 4-bit low part. All in one top-level scope.
    fn sim_with_parts() -> Simulator {
        let scope: ScopeId = 0;
        let sig = |name: &str, width: usize| FlatSig {
            name: name.to_string(),
            width,
            kind: SigKind::Wire,
            init: None,
            mem: None,
            scope,
            signed: false,
            real_ranges: Vec::new(),
        };
        let flat = Flat {
            signals: vec![
                sig("aa", 8),
                sig("bb", 8),
                sig("cc", 1),
                sig("ss", 8),
                sig("ll", 4),
            ],
            scopes: vec![Scope {
                name: String::new(),
                parent: None,
            }],
            ..Flat::default()
        };
        let mut sim = Simulator::new(flat);
        sim.vals[0] = Value::from_u64(8, 100);
        sim.vals[1] = Value::from_u64(8, 55);
        sim
    }

    #[test]
    fn concat_lhs_widens_the_add_to_the_part_total() {
        let sim = sim_with_parts();
        // {cc, ss} = aa + bb — 9-bit context: 100 + 55 keeps bit 8 = 0.
        let lhs = Expr::Concat(vec![Expr::Sig(2), Expr::Sig(3)]);
        let rhs = Expr::Bin {
            op: BinOp::Add,
            l: Box::new(Expr::Sig(0)),
            r: Box::new(Expr::Sig(1)),
        };
        let v = sim.eval_assign_rhs(&lhs, &rhs, None);
        assert_eq!(v.width(), 9, "the add must evaluate at the LHS width");
        assert_eq!(v.to_u64(), Some(155));
        // 200 + 100 = 300: the carry bit (bit 8) is part of the value.
        let mut sim = sim_with_parts();
        sim.vals[0] = Value::from_u64(8, 200);
        sim.vals[1] = Value::from_u64(8, 100);
        let v = sim.eval_assign_rhs(&lhs, &rhs, None);
        assert_eq!(v.width(), 9);
        assert_eq!(v.to_u64(), Some(300));
    }

    #[test]
    fn narrow_concat_lhs_keeps_the_rhs_self_determined_width() {
        let sim = sim_with_parts();
        // {cc, ll} = aa + bb — 5-bit context is below the RHS's own 8-bit
        // self-determined size, so the add stays 8-bit.
        let lhs = Expr::Concat(vec![Expr::Sig(2), Expr::Sig(4)]);
        let rhs = Expr::Bin {
            op: BinOp::Add,
            l: Box::new(Expr::Sig(0)),
            r: Box::new(Expr::Sig(1)),
        };
        let v = sim.eval_assign_rhs(&lhs, &rhs, None);
        assert_eq!(v.width(), 8, "no widening below the RHS's own size");
        assert_eq!(v.to_u64(), Some(155));
    }

    #[test]
    fn plain_signal_lhs_is_not_widened() {
        let sim = sim_with_parts();
        // ss = aa + bb — a non-concat target keeps today's evaluation.
        let rhs = Expr::Bin {
            op: BinOp::Add,
            l: Box::new(Expr::Sig(0)),
            r: Box::new(Expr::Sig(1)),
        };
        let v = sim.eval_assign_rhs(&Expr::Sig(3), &rhs, None);
        assert_eq!(v.width(), 8);
        assert_eq!(v.to_u64(), Some(155));
    }
}
