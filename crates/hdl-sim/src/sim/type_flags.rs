//! Memoized expression type classification for `expr_real`/`expr_signed`.
//!
//! `eval_expr` asks both predicates per `Bin` node (real-typed arithmetic,
//! signed operands), and each answer is a recursive walk of the operand
//! subtree — O(n·depth) per evaluation, re-paid on every delta cycle. Both
//! predicates are pure functions of (subtree, locals context, flat signal
//! metadata), so one walk computes both and a pointer-keyed memo caches
//! them per node.
//!
//! The memo is consulted only while [`Simulator::expr_memo`] is set, i.e.
//! while evaluating trees owned by `flat` (continuous-assign expressions in
//! the fixpoint, function bodies in `run_func`) — those live for the
//! `Simulator`'s lifetime, so their node addresses are stable. Everything
//! else (process bodies are consumed statement-by-statement, procedure
//! bodies clone per call) churns addresses, so it walks uncached.

use super::Simulator;
use hdl_ir::value::Value;
use hdl_ir::{BinOp, Expr, SigKind, UnOp};
use std::collections::HashMap;

/// Memo: node pointer + locals tag → (real, signed). The tag keeps
/// `Expr::Sig` contexts apart: a function frame resolves slot ids from
/// `locals` (never typed), the same node without locals resolves as a
/// flat signal.
pub(super) type FlagMemo =
    HashMap<(*const Expr, u32), (bool, bool), std::hash::BuildHasherDefault<PtrHasher>>;

/// Entry bound: pathological assign trees could pile up entries beyond
/// any useful size; a clear only costs re-walks.
const MEMO_CAP: usize = 1 << 16;

/// Multiply-shift hasher for pointer-keyed entries: one 64-bit mix, no
/// SipHash cost. The map only holds live-`flat`-tree nodes (≤ `MEMO_CAP`),
/// so a single multiply is ample collision resistance.
#[derive(Default)]
pub(super) struct PtrHasher(u64);

impl std::hash::Hasher for PtrHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
        }
    }
    fn write_usize(&mut self, n: usize) {
        self.0 = (n as u64).wrapping_mul(0x9E_37_79_B9_7F_4A_7C_15);
    }
    fn write_u32(&mut self, n: u32) {
        self.0 = (self.0 ^ u64::from(n)).wrapping_mul(0x9E_37_79_B9_7F_4A_7C_15);
    }
}

impl Simulator {
    /// Whether an expression is real-typed (Verilog/VHDL `real`).
    pub(super) fn expr_real(&self, e: &Expr, locals: Option<&Vec<Value>>) -> bool {
        self.expr_flags(e, locals).0
    }

    /// Whether an expression is a signed-typed value (VHDL `signed`).
    pub(super) fn expr_signed(&self, e: &Expr, locals: Option<&Vec<Value>>) -> bool {
        self.expr_flags(e, locals).1
    }

    /// Both flags for `e`, memoized when the enclosing tree is stable
    /// (`expr_memo` — see the module docs).
    #[inline]
    fn expr_flags(&self, e: &Expr, locals: Option<&Vec<Value>>) -> (bool, bool) {
        if !self.expr_memo.get() {
            return self.compute_expr_flags(e, locals);
        }
        let key = (e as *const Expr, locals.map_or(0, |l| l.len() as u32 + 1));
        if let Some(&f) = self.expr_flags_memo.borrow().get(&key) {
            return f;
        }
        let f = self.compute_expr_flags(e, locals);
        let mut memo = self.expr_flags_memo.borrow_mut();
        if memo.len() >= MEMO_CAP {
            memo.clear();
        }
        memo.insert(key, f);
        f
    }

    fn compute_expr_flags(&self, e: &Expr, locals: Option<&Vec<Value>>) -> (bool, bool) {
        match e {
            Expr::Real(_) => (true, false),
            Expr::Signed(e) => (self.expr_flags(e, locals).0, true),
            Expr::Sig(s) => {
                let i = *s as usize;
                if locals.is_some_and(|l| i < l.len()) {
                    return (false, false); // function/variable slots carry no type metadata
                }
                match self.flat.signals.get(i) {
                    Some(s) => (s.kind == SigKind::Real, s.signed),
                    None => (false, false),
                }
            }
            Expr::Sel { e, msb, lsb } => (self.sel_real(e, msb, lsb, locals), false),
            Expr::Bin { op, l, r } => self.bin_flags(*op, l, r, locals),
            Expr::Un { op, e } => {
                let (r, s) = self.expr_flags(e, locals);
                (matches!(op, UnOp::Neg | UnOp::Pos | UnOp::Abs) && r, s)
            }
            Expr::Resize { e, signed, .. } => {
                let (r, s) = self.expr_flags(e, locals);
                (r, *signed || s)
            }
            Expr::Cond { c, t, e } => {
                let cr = self.expr_flags(c, locals).0;
                let tr = self.expr_flags(t, locals).0;
                let er = self.expr_flags(e, locals).0;
                (cr || tr || er, false)
            }
            _ => (false, false),
        }
    }

    fn bin_flags(
        &self,
        op: BinOp,
        l: &Expr,
        r: &Expr,
        locals: Option<&Vec<Value>>,
    ) -> (bool, bool) {
        let (lr, ls) = self.expr_flags(l, locals);
        let (rr, rs) = self.expr_flags(r, locals);
        // Only arithmetic propagates realness; comparisons/logical ops
        // yield 1-bit results even on real operands. Shifts/rotates
        // preserve the operand's signedness (the amount is just a count);
        // other binops need both sides signed.
        let real = matches!(
            op,
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod
        ) && (lr || rr);
        let signed = if matches!(
            op,
            BinOp::Shl | BinOp::Shr | BinOp::Sar | BinOp::Rol | BinOp::Ror
        ) {
            ls
        } else {
            ls && rs
        };
        (real, signed)
    }

    /// Record-field select on a real-typed field (`r.re`): decode the
    /// slice as IEEE-754 instead of an integer bit pattern. Constant
    /// bounds only — the lowerer emits literals for record fields.
    fn sel_real(&self, e: &Expr, msb: &Expr, lsb: &Expr, locals: Option<&Vec<Value>>) -> bool {
        let (Expr::Sig(s), Some(m), Some(l)) = (
            e,
            self.const_bound(msb, locals),
            self.const_bound(lsb, locals),
        ) else {
            return false;
        };
        let i = *s as usize;
        if locals.is_some_and(|lc| i < lc.len()) {
            return false;
        }
        self.flat
            .signals
            .get(i)
            .is_some_and(|s| s.real_ranges.iter().any(|&(lo, hi)| lo == l && hi == m))
    }

    /// Constant bound of a select (record-field bounds are literals from
    /// the lowerer); dynamic bounds conservatively report None.
    fn const_bound(&self, e: &Expr, locals: Option<&Vec<Value>>) -> Option<u32> {
        if matches!(e, Expr::Lit(_)) {
            return self.eval_expr(e, locals).to_u64().map(|v| v as u32);
        }
        None
    }
}
