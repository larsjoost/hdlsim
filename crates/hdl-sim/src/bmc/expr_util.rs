//! Small pure `Expr` helpers for the BMC engine: LHS root resolution
//! and don't-care ('-') detection. Pure move from `bmc.rs`.

use hdl_ir::elab::FlatSigId;
use hdl_ir::value::VDASH;
use hdl_ir::Expr;

pub(super) fn sig_root(e: &Expr) -> Result<FlatSigId, String> {
    match e {
        Expr::Sig(s) => Ok(*s),
        Expr::Idx { e: x, .. } | Expr::Sel { e: x, .. } => sig_root(x),
        _ => Err("prove: unsupported LHS root".to_string()),
    }
}

/// Does this expression contain a don't-care ('-') literal bit?
/// Does this expression contain a don't-care ('-') literal bit?
pub(super) fn expr_has_dash(e: &Expr) -> bool {
    match e {
        Expr::SigAttr { .. } => false,
        Expr::Lit(v) => v.bits.contains(&VDASH),
        Expr::Bin { l, r, .. } => expr_has_dash(l) || expr_has_dash(r),
        Expr::Un { e, .. } => expr_has_dash(e),
        Expr::Cond { c, t, e } => expr_has_dash(c) || expr_has_dash(t) || expr_has_dash(e),
        Expr::Call { args, .. } => args.iter().any(expr_has_dash),
        Expr::Concat(parts) => parts.iter().any(expr_has_dash),
        Expr::Replicate { count, e } => expr_has_dash(count) || expr_has_dash(e),
        Expr::Sel { e, msb, lsb } => expr_has_dash(e) || expr_has_dash(msb) || expr_has_dash(lsb),
        Expr::Idx { e, i } => expr_has_dash(e) || expr_has_dash(i),
        Expr::Rising(e)
        | Expr::Falling(e)
        | Expr::Prev(e)
        | Expr::ToInt(e)
        | Expr::ToString { e, .. } => expr_has_dash(e),
        Expr::Resize { e, .. } | Expr::Signed(e) => expr_has_dash(e),
        Expr::Sig(_) | Expr::Real(_) => false,
    }
}
