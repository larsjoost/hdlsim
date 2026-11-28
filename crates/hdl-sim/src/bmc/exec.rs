//! Symbolic execution over CNF literals: the statement interpreter, branch
//! state merging, and assignment-LHS write planning. Split out of `bmc.rs`
//! (expression evaluation lives in `exec_expr`, constant folding in
//! `const_fold`).

use super::bitvec::{mux_vec, or_bits, resize_bits};
use super::const_fold::{const_bits, const_truthy};
use super::encode::Encoder;
use super::expr_util::{expr_has_dash, sig_root};
use crate::sat::Lit;
use hdl_ir::elab::FlatSigId;
use hdl_ir::value::Value;
use hdl_ir::{BinOp, Expr, Stmt};
use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Ctrl {
    Done,
    Break,
    Continue,
}

/// A symbolic value: bit literals + signedness.
pub(super) struct Val {
    pub(super) bits: Vec<Lit>,
    pub(super) signed: bool,
}

pub(super) struct Exec<'e, 'a> {
    pub(super) enc: &'e mut Encoder<'a>,
    pub(super) t: usize,
    pub(super) env: HashMap<FlatSigId, Vec<Lit>>,
    /// Function slot space (args then locals) while executing a function body.
    pub(super) locals: Option<HashMap<u32, Vec<Lit>>>,
    /// Non-blocking assign results, applied at the end of the time step.
    /// Kept separately from `env` so if/case merges can mux both.
    pub(super) nba_env: HashMap<FlatSigId, Vec<Lit>>,
    /// During comb encoding: reading a not-yet-encoded comb signal is an error.
    pub(super) strict_comb: bool,
}

impl<'e, 'a> Exec<'e, 'a> {
    pub(super) fn at(enc: &'e mut Encoder<'a>, t: usize) -> Exec<'e, 'a> {
        Exec {
            enc,
            t,
            env: HashMap::new(),
            locals: None,
            nba_env: HashMap::new(),
            strict_comb: false,
        }
    }

    pub(super) fn comb(enc: &'e mut Encoder<'a>, t: usize) -> Exec<'e, 'a> {
        let mut e = Exec::at(enc, t);
        e.strict_comb = true;
        e
    }

    fn width(&self, s: FlatSigId) -> usize {
        self.enc.width(s)
    }

    pub(super) fn val(&self, s: FlatSigId) -> Result<Vec<Lit>, String> {
        if let Some(l) = &self.locals {
            if let Some(v) = l.get(&s) {
                return Ok(v.clone());
            }
        }
        if let Some(v) = self.env.get(&s) {
            return Ok(v.clone());
        }
        self.val_base(s, None)
    }

    /// Current value with an extra base layer (used for NBA part-writes, where
    /// the base is the already-recorded NBA value).
    fn val_base(
        &self,
        s: FlatSigId,
        extra: Option<&HashMap<FlatSigId, Vec<Lit>>>,
    ) -> Result<Vec<Lit>, String> {
        if let Some(e) = extra {
            if let Some(v) = e.get(&s) {
                return Ok(v.clone());
            }
        }
        if self.strict_comb && self.enc.cls.is_comb(s) && !self.enc.comb_done[self.t].contains(&s) {
            return Err(format!(
                "prove: combinational signal '{}' read before its driver (cycle?)",
                self.enc.flat.signals[s as usize].name
            ));
        }
        Ok(self.enc.lits[self.t][s as usize].clone())
    }

    // --- statements ---

    pub(super) fn exec_stmt(&mut self, s: &Stmt) -> Result<Ctrl, String> {
        match s {
            Stmt::Spanned { inner, .. } => self.exec_stmt(inner),
            Stmt::Null => Ok(Ctrl::Done),
            // Functions aren't executed in BMC; treat return as done.
            Stmt::Return(_) => Ok(Ctrl::Done),
            Stmt::Block(ss) => self.exec_block(ss),
            Stmt::If { cond, then, els } => self.exec_if(cond, then, els.as_deref()),
            Stmt::Case {
                expr,
                items,
                default,
                matching,
            } => self.exec_case(expr, items, default, *matching),
            Stmt::Assign { lhs, rhs, nba } => self.exec_assign(lhs, rhs, *nba),
            // Procedural continuous assignment/deassign are time-persistent
            // state; BMC's unrolled frames do not model them.
            Stmt::ProcContAssign { .. } => {
                Err("prove: procedural continuous assignment is not supported".to_string())
            }
            Stmt::Deassign { .. } => Err("prove: deassign is not supported in prove".to_string()),
            Stmt::For {
                init,
                cond,
                step,
                body,
            } => self.exec_for(init, cond, step, body),
            Stmt::Repeat { count, body } => self.exec_repeat(count, body),
            Stmt::Break => Ok(Ctrl::Break),
            Stmt::Continue => Ok(Ctrl::Continue),
            Stmt::ExprStmt(e) => {
                let _ = self.eval(e)?;
                Ok(Ctrl::Done)
            }
            Stmt::SysCall { name, .. } if name == "sva_assert" || name == "assert" => {
                // Property check: extracted separately, no transition effect.
                Ok(Ctrl::Done)
            }
            Stmt::SysCall { name, .. } => Err(format!(
                "prove: system task '${}' is not supported in processes (only asserts)",
                name
            )),
            Stmt::Delay { .. } => Err("prove: `#delay` is not supported".to_string()),
            Stmt::AssignDelayed { .. } => {
                Err("prove: delayed signal assignment (`after`) is not supported".to_string())
            }
            Stmt::Wait { .. } => Err("prove: `@wait` is not supported in processes".to_string()),
            Stmt::WaitUntil { .. } => {
                Err("prove: `wait until` is not supported in processes".to_string())
            }
        }
    }

    fn exec_block(&mut self, ss: &[Stmt]) -> Result<Ctrl, String> {
        for s in ss {
            let c = self.exec_stmt(s)?;
            if c != Ctrl::Done {
                return Ok(c);
            }
        }
        Ok(Ctrl::Done)
    }

    fn exec_if(&mut self, cond: &Expr, then: &Stmt, els: Option<&Stmt>) -> Result<Ctrl, String> {
        let cv = self.eval(cond)?;
        let c = or_bits(&mut self.enc.b, &cv.bits);
        let saved = self.env.clone();
        let saved_nba = self.nba_env.clone();
        let ctrl_t = self.exec_stmt(then)?;
        let env_t = std::mem::replace(&mut self.env, saved.clone());
        let nba_t = std::mem::replace(&mut self.nba_env, saved_nba.clone());
        if ctrl_t != Ctrl::Done {
            self.env = env_t;
            self.nba_env = nba_t;
            return Ok(ctrl_t);
        }
        let ctrl_e = match els {
            Some(e) => self.exec_stmt(e)?,
            None => Ctrl::Done,
        };
        let env_e = std::mem::replace(&mut self.env, saved.clone());
        let nba_e = std::mem::replace(&mut self.nba_env, saved_nba.clone());
        if ctrl_e != Ctrl::Done {
            self.env = env_e;
            self.nba_env = nba_e;
            return Ok(ctrl_e);
        }
        self.merge_envs(c, &env_t, &env_e, &saved, &nba_t, &nba_e, &saved_nba)?;
        Ok(Ctrl::Done)
    }

    fn exec_case(
        &mut self,
        expr: &Expr,
        items: &[(Vec<Expr>, Stmt)],
        default: &Option<Box<Stmt>>,
        matching: bool,
    ) -> Result<Ctrl, String> {
        if matching && case_has_dash(items) {
            return Err("matching case ('-') unsupported in BMC".to_string());
        }
        match desugar_case(expr, items, default.as_deref()) {
            Some(c) => self.exec_stmt(&c),
            None => Ok(Ctrl::Done),
        }
    }

    fn exec_assign(&mut self, lhs: &Expr, rhs: &Expr, nba: bool) -> Result<Ctrl, String> {
        let v = self.eval(rhs)?;
        if nba {
            // NBA: RHS is evaluated now; the write lands in the NBA
            // region (nba_env), on top of any earlier NBA to the same
            // signal. A later NBA overrides an earlier blocking write.
            let updates = self.plan_write_base(lhs, &v.bits, Some(&self.nba_env))?;
            for (s, bits) in updates {
                self.nba_env.insert(s, bits);
            }
        } else {
            let updates = self.plan_write(lhs, &v.bits)?;
            for (s, bits) in updates {
                self.env.insert(s, bits);
            }
        }
        Ok(Ctrl::Done)
    }

    fn exec_for(
        &mut self,
        init: &Stmt,
        cond: &Expr,
        step: &Stmt,
        body: &Stmt,
    ) -> Result<Ctrl, String> {
        self.exec_stmt(init)?;
        let mut iter = 0usize;
        loop {
            let cv = self.eval(cond)?;
            let Some(cb) = const_bits(&cv.bits, self) else {
                return Err(
                    "prove: for-loop with non-constant condition is not supported".to_string(),
                );
            };
            if !const_truthy(&cb) {
                break;
            }
            let ctrl = self.exec_stmt(body)?;
            if ctrl == Ctrl::Break {
                break;
            }
            self.exec_stmt(step)?;
            iter += 1;
            if iter > 100_000 {
                return Err("prove: for-loop exceeds 100000 iterations".to_string());
            }
        }
        Ok(Ctrl::Done)
    }

    fn exec_repeat(&mut self, count: &Expr, body: &Stmt) -> Result<Ctrl, String> {
        let Some(n) = self.const_u64(count) else {
            return Err("prove: repeat with non-constant count is not supported".to_string());
        };
        for _ in 0..n.min(100_000) {
            let ctrl = self.exec_stmt(body)?;
            if ctrl == Ctrl::Break {
                break;
            }
        }
        Ok(Ctrl::Done)
    }

    /// Merge two branch states under condition `c`: written keys become
    /// c ? then-value : else-value; unwritten keys keep the base value.
    /// Merges both the blocking env and the NBA env.
    // BMC state threading: the two envs and their NBA twins; a params
    // struct would just relocate the same fields.
    #[allow(clippy::too_many_arguments)]
    fn merge_envs(
        &mut self,
        c: Lit,
        env_t: &HashMap<FlatSigId, Vec<Lit>>,
        env_e: &HashMap<FlatSigId, Vec<Lit>>,
        base: &HashMap<FlatSigId, Vec<Lit>>,
        nba_t: &HashMap<FlatSigId, Vec<Lit>>,
        nba_e: &HashMap<FlatSigId, Vec<Lit>>,
        nba_base: &HashMap<FlatSigId, Vec<Lit>>,
    ) -> Result<(), String> {
        self.merge_map(c, env_t, env_e, base, true)?;
        self.merge_map(c, nba_t, nba_e, nba_base, false)?;
        Ok(())
    }

    fn merge_map(
        &mut self,
        c: Lit,
        map_t: &HashMap<FlatSigId, Vec<Lit>>,
        map_e: &HashMap<FlatSigId, Vec<Lit>>,
        base: &HashMap<FlatSigId, Vec<Lit>>,
        into_env: bool,
    ) -> Result<(), String> {
        let mut keys: Vec<FlatSigId> = map_t.keys().chain(map_e.keys()).copied().collect();
        keys.sort_unstable();
        keys.dedup();
        for k in keys {
            let vt = map_t.get(&k).cloned();
            let ve = map_e.get(&k).cloned();
            let bv = match base.get(&k) {
                Some(v) => v.clone(),
                None => self.val(k)?,
            };
            let (a, b) = match (&vt, &ve) {
                (Some(a), Some(b)) => (a.clone(), b.clone()),
                (Some(a), None) => (a.clone(), bv.clone()),
                (None, Some(b)) => (bv.clone(), b.clone()),
                (None, None) => continue,
            };
            let bits = if a == b {
                a
            } else {
                mux_vec(&mut self.enc.b, c, &a, &b)
            };
            if into_env {
                self.env.insert(k, bits);
            } else {
                self.nba_env.insert(k, bits);
            }
        }
        Ok(())
    }

    /// Plan a write of `bits` to `lhs`: returns (sig, new value) pairs, reading
    /// current values through env / step-t lits.
    fn plan_write(&self, lhs: &Expr, bits: &[Lit]) -> Result<Vec<(FlatSigId, Vec<Lit>)>, String> {
        self.plan_write_base(lhs, bits, None)
    }

    /// Same, with an extra base layer (NBA env) consulted before `env`.
    fn plan_write_base(
        &self,
        lhs: &Expr,
        bits: &[Lit],
        extra: Option<&HashMap<FlatSigId, Vec<Lit>>>,
    ) -> Result<Vec<(FlatSigId, Vec<Lit>)>, String> {
        match lhs {
            Expr::Sig(s) => {
                let w = self.width(*s);
                Ok(vec![(*s, resize_bits(&self.enc.b, bits, w))])
            }
            Expr::Idx { e, i } => self.plan_write_idx(e, i, bits, extra),
            Expr::Sel { e, msb, lsb } => self.plan_write_sel(e, msb, lsb, bits, extra),
            Expr::Concat(parts) => self.plan_write_concat(parts, bits, extra),
            _ => Err("prove: unsupported assignment LHS".to_string()),
        }
    }

    /// Bit-select write: replace one bit of the current value.
    fn plan_write_idx(
        &self,
        e: &Expr,
        i: &Expr,
        bits: &[Lit],
        extra: Option<&HashMap<FlatSigId, Vec<Lit>>>,
    ) -> Result<Vec<(FlatSigId, Vec<Lit>)>, String> {
        let root = sig_root(e)?;
        if self.enc.flat.signals[root as usize].mem.is_some() {
            return Err("prove: memory write not supported".to_string());
        }
        let a = self
            .const_u64(i)
            .ok_or("prove: non-constant bit index is not supported")? as usize;
        let mut cur = self.val_base(root, extra)?;
        let w = self.width(root);
        if a < w {
            cur[a] = resize_bits(&self.enc.b, bits, 1)[0];
        }
        Ok(vec![(root, cur)])
    }

    /// Part-select write: replace bits `msb..=lsb` of the current value.
    fn plan_write_sel(
        &self,
        e: &Expr,
        msb: &Expr,
        lsb: &Expr,
        bits: &[Lit],
        extra: Option<&HashMap<FlatSigId, Vec<Lit>>>,
    ) -> Result<Vec<(FlatSigId, Vec<Lit>)>, String> {
        let root = sig_root(e)?;
        let m = self
            .const_u64(msb)
            .ok_or("prove: non-constant part-select is not supported")? as usize;
        let l = self
            .const_u64(lsb)
            .ok_or("prove: non-constant part-select is not supported")? as usize;
        let mut cur = self.val_base(root, extra)?;
        let w = self.width(root);
        let msb = m.min(w.saturating_sub(1));
        let lsb = l.min(msb);
        let part = resize_bits(&self.enc.b, bits, msb - lsb + 1);
        for (k, &b) in part.iter().enumerate() {
            cur[lsb + k] = b;
        }
        Ok(vec![(root, cur)])
    }

    /// Concatenation LHS: slice the value across the parts, MSB first.
    fn plan_write_concat(
        &self,
        parts: &[Expr],
        bits: &[Lit],
        extra: Option<&HashMap<FlatSigId, Vec<Lit>>>,
    ) -> Result<Vec<(FlatSigId, Vec<Lit>)>, String> {
        let mut widths = Vec::new();
        for p in parts {
            widths.push(self.lhs_width(p)?);
        }
        let total: usize = widths.iter().sum();
        let bits = resize_bits(&self.enc.b, bits, total);
        let mut out = Vec::new();
        let mut shift = 0usize;
        for (p, w) in parts.iter().zip(widths.iter()) {
            let sub: Vec<Lit> = bits[shift..shift + *w].to_vec();
            out.extend(self.plan_write_base(p, &sub, extra)?);
            shift += *w;
        }
        Ok(out)
    }

    fn lhs_width(&self, lhs: &Expr) -> Result<usize, String> {
        match lhs {
            Expr::Sig(s) => Ok(self.width(*s)),
            Expr::Idx { .. } => Ok(1),
            Expr::Sel { msb, lsb, .. } => {
                let m = self.const_u64(msb).ok_or("prove: non-constant select")? as usize;
                let l = self.const_u64(lsb).ok_or("prove: non-constant select")? as usize;
                Ok(m.saturating_sub(l) + 1)
            }
            Expr::Concat(parts) => {
                let mut t = 0;
                for p in parts {
                    t += self.lhs_width(p)?;
                }
                Ok(t)
            }
            _ => Err("prove: unsupported LHS in concat".to_string()),
        }
    }
}

/// BMC is 2-valued: matching-case wildcards ('-') have no SAT meaning.
fn case_has_dash(items: &[(Vec<Expr>, Stmt)]) -> bool {
    items
        .iter()
        .any(|(conds, _)| conds.iter().any(expr_has_dash))
}

/// Desugar a case statement to a priority if-chain: first matching item wins.
fn desugar_case(
    expr: &Expr,
    items: &[(Vec<Expr>, Stmt)],
    default: Option<&Stmt>,
) -> Option<Box<Stmt>> {
    let mut chain: Option<Box<Stmt>> = default.map(|d| Box::new(d.clone()));
    for (conds, body) in items.iter().rev() {
        let mut cond: Option<Expr> = None;
        for c in conds {
            let e = Expr::Bin {
                op: BinOp::Eq,
                l: Box::new(expr.clone()),
                r: Box::new(c.clone()),
            };
            cond = Some(match cond {
                None => e,
                Some(prev) => Expr::Bin {
                    op: BinOp::LogOr,
                    l: Box::new(prev),
                    r: Box::new(e),
                },
            });
        }
        let cond = cond.unwrap_or_else(|| Expr::Lit(Value::from_u64(1, 0)));
        chain = Some(Box::new(Stmt::If {
            cond,
            then: Box::new(body.clone()),
            els: chain,
        }));
    }
    chain
}
