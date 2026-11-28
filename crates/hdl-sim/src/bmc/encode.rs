//! Step encoding for BMC: fresh CNF variables per signal and step, the
//! combinational fixpoint, step-0 init, and the clocked transition
//! relation. Pure move from `bmc.rs`.

use super::bitvec::resize_bits;
use super::builder::Builder;
use super::classify::{Class, ProcCls, Writer};
use super::{Ctrl, Exec};
use crate::sat::Lit;
use hdl_ir::elab::{Flat, FlatSigId};
use hdl_ir::value::V1;
use hdl_ir::Expr;
use std::collections::HashSet;

pub(super) struct Encoder<'a> {
    pub(super) b: Builder,
    pub(super) flat: &'a Flat,
    pub(super) cls: Class,
    /// lits[t][sig][bit]
    pub(super) lits: Vec<Vec<Vec<Lit>>>,
    pub(super) comb_done: Vec<HashSet<FlatSigId>>,
}

impl<'a> Encoder<'a> {
    pub(super) fn new(flat: &'a Flat, cls: Class, steps: usize) -> Result<Encoder<'a>, String> {
        let mut b = Builder::new();
        let mut lits = Vec::with_capacity(steps + 1);
        for _t in 0..=steps {
            let mut row: Vec<Vec<Lit>> = Vec::with_capacity(flat.signals.len());
            for sig in &flat.signals {
                let w = sig.width.max(1);
                let mut bits = Vec::with_capacity(w);
                for _ in 0..w {
                    bits.push(b.fresh());
                }
                row.push(bits);
            }
            lits.push(row);
        }
        Ok(Encoder {
            b,
            flat,
            cls,
            lits,
            comb_done: vec![HashSet::new(); steps + 1],
        })
    }

    pub(super) fn width(&self, s: FlatSigId) -> usize {
        self.flat.signals[s as usize].width.max(1)
    }

    /// Encode all combinational writers at step t (topological order).
    pub(super) fn encode_comb(&mut self, t: usize) -> Result<(), String> {
        let order = self.cls.comb_order.clone();
        for w in order {
            match w {
                Writer::Assign(ai) => {
                    let a = self.flat.assigns[ai].clone();
                    let Expr::Sig(s) = &a.lhs else {
                        return Err("prove: bad continuous assign LHS".to_string());
                    };
                    let s = *s;
                    let mut ex = Exec::comb(self, t);
                    let v = ex.eval(&a.rhs)?;
                    let bits = resize_bits(&self.b, &v.bits, self.width(s));
                    self.lits[t][s as usize] = bits;
                    self.comb_done[t].insert(s);
                }
                Writer::Proc(pi) => {
                    let body = self.flat.procs[pi].body.clone();
                    let writes = self.cls.procs[pi].1.clone();
                    let results: Vec<(FlatSigId, Vec<Lit>)> = {
                        let mut ex = Exec::comb(self, t);
                        ex.exec_stmt(&body)?;
                        let mut out = Vec::new();
                        for s in &writes {
                            let bits = match ex.env.get(s) {
                                Some(v) => v.clone(),
                                None => ex.val(*s)?,
                            };
                            out.push((*s, bits));
                        }
                        out
                    };
                    for (s, v) in results {
                        let bits = resize_bits(&self.b, &v, self.width(s));
                        self.lits[t][s as usize] = bits;
                        self.comb_done[t].insert(s);
                    }
                }
            }
        }
        Ok(())
    }

    /// Step-0 init: declaration init values, then initial blocks.
    pub(super) fn encode_init(&mut self) -> Result<(), String> {
        let state = self.cls.state.clone();
        let procs = self.cls.procs.clone();
        let mut init_written: HashSet<FlatSigId> = HashSet::new();
        for (pc, writes) in &procs {
            if *pc == ProcCls::Init {
                init_written.extend(writes.iter().copied());
            }
        }
        for s in &state {
            if init_written.contains(s) {
                continue; // initial blocks override declaration init
            }
            if let Some(v) = &self.flat.signals[*s as usize].init {
                for (i, &bit) in v.bits.iter().enumerate() {
                    if i < self.width(*s) {
                        let l = self.b.const_lit(bit == V1);
                        let eq = self.b.eq_lit(self.lits[0][*s as usize][i], l);
                        self.b.clause(&[eq]);
                    }
                }
            }
        }
        for (pi, (pc, writes)) in procs.iter().enumerate() {
            if *pc != ProcCls::Init {
                continue;
            }
            let body = self.flat.procs[pi].body.clone();
            let results: Vec<(FlatSigId, Vec<Lit>)> = {
                let mut ex = Exec::at(self, 0);
                ex.exec_stmt(&body)?;
                let mut out = Vec::new();
                for s in writes {
                    if let Some(v) = ex.env.get(s) {
                        out.push((*s, v.clone()));
                    }
                }
                out
            };
            for (s, v) in results {
                let w = self.width(s);
                let bits = resize_bits(&self.b, &v, w);
                for (i, &bit) in bits.iter().enumerate().take(w) {
                    let eq = self.b.eq_lit(self.lits[0][s as usize][i], bit);
                    self.b.clause(&[eq]);
                }
            }
        }
        Ok(())
    }

    /// Transition t → t+1: run all clocked processes at step t.
    pub(super) fn encode_transition(&mut self, t: usize) -> Result<(), String> {
        let procs = self.cls.procs.clone();
        let mut written: HashSet<FlatSigId> = HashSet::new();
        for (pi, (pc, writes)) in procs.iter().enumerate() {
            if *pc != ProcCls::Clocked || writes.is_empty() {
                continue; // property-only procs (e.g. `|=>`) write nothing
            }
            let body = self.flat.procs[pi].body.clone();
            let nexts: Vec<(FlatSigId, Vec<Lit>)> = {
                let mut ex = Exec::at(self, t);
                let ctrl = ex.exec_stmt(&body)?;
                if ctrl != Ctrl::Done {
                    return Err("prove: break/continue outside a loop".to_string());
                }
                // NBA results take precedence over blocking writes (they are
                // applied in the NBA region, after the active region).
                let mut out = Vec::new();
                for s in writes.iter() {
                    let next = match ex.nba_env.get(s).or_else(|| ex.env.get(s)) {
                        Some(v) => v.clone(),
                        None => ex.val(*s)?,
                    };
                    out.push((*s, next));
                }
                out
            };
            for (s, v) in nexts {
                written.insert(s);
                let w = self.width(s);
                let bits = resize_bits(&self.b, &v, w);
                for (i, &bit) in bits.iter().enumerate().take(w) {
                    let eq = self.b.eq_lit(self.lits[t + 1][s as usize][i], bit);
                    self.b.clause(&[eq]);
                }
            }
        }
        // Unwritten state holds its value.
        let state = self.cls.state.clone();
        for s in state {
            if written.contains(&s) {
                continue;
            }
            let w = self.width(s);
            for i in 0..w {
                let eq = self
                    .b
                    .eq_lit(self.lits[t + 1][s as usize][i], self.lits[t][s as usize][i]);
                self.b.clause(&[eq]);
            }
        }
        Ok(())
    }
}
