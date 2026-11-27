//! Assignment execution: writing a value to an assign target — a whole
//! signal, a memory word, a bit, or a slice — plus the continuous-assign
//! fixpoint sweep's target machinery and the memory-grow RSS guard.
//!
//! Child module of `sim` so it can read the kernel's private fields, the
//! same pattern as `sim/probe.rs`.

use super::Simulator;
use crate::events::{MessageSeverity, SimEvent};
use hdl_ir::elab::FlatSigId;
use hdl_ir::value::Value;
use hdl_ir::Expr;

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
}
