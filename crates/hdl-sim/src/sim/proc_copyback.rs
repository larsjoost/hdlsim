//! Procedure `inout`/`out` formal copy-back: on return, a caller's `variable`
//! actual receives the formal's final value (LRM 1076 §4.3). Child module of
//! `sim` so it can reach the kernel's private assign/eval helpers.
//!
//! Two caller shapes exist. A nested call's actual names the caller's frame
//! slot (`Expr::Sig`) — whole, element (`v(i)`), or slice (`v(hi downto lo)`)
//! — spliced directly. A process-level call has no frame (process variables
//! are flat signals), so the write goes through the blocking `assign_to`,
//! immediate like the `:=` the caller would have written.

use hdl_ir::value::{Value, VX};
use hdl_ir::{ArgMode, Expr};

use super::Simulator;

impl Simulator {
    /// Copy each `inout`/`out` formal's final value back to its actual.
    /// `in` formals never copy back — the actual may be an expression.
    pub(super) fn copy_back_formals(
        &mut self,
        modes: &[ArgMode],
        args: &[Expr],
        lvals: &[Value],
        caller: Option<&mut Vec<Value>>,
    ) {
        let Some(frame) = caller else {
            for (i, a) in args.iter().enumerate() {
                if matches!(modes.get(i), Some(ArgMode::Inout | ArgMode::Out)) {
                    if let Some(v) = lvals.get(i + 1) {
                        self.assign_to(a, v.clone(), None);
                    }
                }
            }
            return;
        };
        for (i, a) in args.iter().enumerate() {
            if matches!(modes.get(i), Some(ArgMode::Inout | ArgMode::Out)) {
                if let Some(v) = lvals.get(i + 1).cloned() {
                    self.copy_back_frame(a, v, frame);
                }
            }
        }
    }

    /// Splice one formal's final value into the caller's frame slot named by
    /// `actual` (whole slot, element, or slice; other expressions are not
    /// variable targets and cannot receive a copy-back).
    fn copy_back_frame(&mut self, actual: &Expr, v: Value, frame: &mut Vec<Value>) {
        let (base, hi_b, lo_b): (&Expr, &Expr, &Expr) = match actual {
            Expr::Sig(_) => (actual, actual, actual),
            Expr::Idx { e, i } => (e, i, i),
            Expr::Sel { e, msb, lsb } => (e, msb, lsb),
            _ => return,
        };
        let Expr::Sig(s) = base else { return };
        let s = *s as usize;
        if s >= frame.len() {
            return;
        }
        if matches!(actual, Expr::Sig(_)) {
            frame[s] = v.resize(frame[s].width().max(1));
            return;
        }
        // Element (`v(i)`) or slice (`v(hi downto lo)`) actual: splice at the
        // bounds as they evaluate now, growing the slot with X if needed.
        let (Some(m), Some(l)) = (
            self.eval_expr(hi_b, Some(frame)).to_u64(),
            self.eval_expr(lo_b, Some(frame)).to_u64(),
        ) else {
            return;
        };
        let (m, l) = (m as usize, l as usize);
        let w = if matches!(actual, Expr::Sel { .. }) {
            m.saturating_sub(l) + 1
        } else {
            v.width().max(1)
        };
        if frame[s].bits.len() < l + w {
            let mut bits = frame[s].bits.clone();
            bits.resize(l + w, VX);
            frame[s] = Value { bits, real: false };
        }
        for b in 0..w {
            frame[s].set_bit(l + b, v.bit(b));
        }
    }
}
