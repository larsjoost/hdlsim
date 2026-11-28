//! IEEE-754 evaluation for real-typed expressions: operand decoding,
//! arithmetic/comparison, and constant select bounds.
//!
//! Child module of `sim` so it can read the kernel's private fields, the
//! same pattern as `sim/probe.rs`.

use super::Simulator;
use hdl_ir::value::Value;
use hdl_ir::{BinOp, Expr};

impl Simulator {
    /// Constant bound of a select (record-field bounds are literals from the
    /// lowerer); dynamic bounds conservatively report None.
    pub(super) fn const_bound(&self, e: &Expr, locals: Option<&Vec<Value>>) -> Option<u32> {
        if matches!(e, Expr::Lit(_)) {
            return self.eval_expr(e, locals).to_u64().map(|v| v as u32);
        }
        None
    }

    /// One operand of a real-typed binop as f64: real operands decode their
    /// bit pattern, integer operands convert their integer value.
    fn operand_f64(&self, e: &Expr, v: &Value, locals: Option<&Vec<Value>>) -> Option<f64> {
        if self.expr_real(e, locals) || v.real {
            v.to_f64()
        } else {
            v.to_u64().map(|n| n as f64)
        }
    }

    /// IEEE-754 arithmetic/comparison for real-typed binops.
    pub(super) fn eval_bin_real(
        &self,
        op: BinOp,
        l: &Expr,
        r: &Expr,
        lv: &Value,
        rv: &Value,
        locals: Option<&Vec<Value>>,
    ) -> Value {
        let a = self.operand_f64(l, lv, locals);
        let b = self.operand_f64(r, rv, locals);
        match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => match (a, b) {
                (Some(x), Some(y)) => Value::from_f64(match op {
                    BinOp::Add => x + y,
                    BinOp::Sub => x - y,
                    BinOp::Mul => x * y,
                    BinOp::Div => x / y,
                    _ => x % y,
                }),
                _ => Value::x(64),
            },
            BinOp::Lt
            | BinOp::Le
            | BinOp::Gt
            | BinOp::Ge
            | BinOp::Eq
            | BinOp::Ne
            | BinOp::EqStrict
            | BinOp::NeStrict => match (a, b) {
                (Some(x), Some(y)) => Value::from_u64(
                    1,
                    match op {
                        BinOp::Lt => x < y,
                        BinOp::Le => x <= y,
                        BinOp::Gt => x > y,
                        BinOp::Ge => x >= y,
                        // Reals carry no metavalues: the strict variants
                        // (VHDL `=`/`/=` lowering) compare identically.
                        BinOp::Eq | BinOp::EqStrict => x == y,
                        _ => x != y,
                    } as u64,
                ),
                _ => Value::x(1),
            },
            _ => Value::x(64), // bitwise/shift ops on reals: undefined
        }
    }
}
