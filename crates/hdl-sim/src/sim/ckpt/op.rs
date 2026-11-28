//! Checkpoint codec for the closed operator enums (`BinOp`, `UnOp`):
//! one tag byte per variant, matching the wire order of the `Expr`/`Stmt`
//! codecs in the parent module. Pure move from `ckpt.rs`.

use super::*;
use hdl_ir::{BinOp, UnOp};

pub(super) fn w_binop(w: &mut W, op: BinOp) {
    w.u8(match op {
        BinOp::Add => 0,
        BinOp::Sub => 1,
        BinOp::Mul => 2,
        BinOp::Div => 3,
        BinOp::Mod => 4,
        BinOp::Shl => 5,
        BinOp::Shr => 6,
        BinOp::Sar => 7,
        BinOp::Lt => 8,
        BinOp::Le => 9,
        BinOp::Gt => 10,
        BinOp::Ge => 11,
        BinOp::Eq => 12,
        BinOp::Ne => 13,
        BinOp::EqStrict => 14,
        BinOp::NeStrict => 15,
        BinOp::BAnd => 16,
        BinOp::BOr => 17,
        BinOp::BXor => 18,
        BinOp::BNand => 19,
        BinOp::BNor => 20,
        BinOp::BXnor => 21,
        BinOp::LogAnd => 22,
        BinOp::LogOr => 23,
        BinOp::EqM => 24,
        BinOp::NeM => 25,
        BinOp::LtM => 26,
        BinOp::LeM => 27,
        BinOp::GtM => 28,
        BinOp::GeM => 29,
        BinOp::Rol => 30,
        BinOp::Ror => 31,
        BinOp::Pow => 32,
    })
}
pub(super) fn r_binop(r: &mut R) -> Result<BinOp, String> {
    Ok(match r.u8()? {
        0 => BinOp::Add,
        1 => BinOp::Sub,
        2 => BinOp::Mul,
        3 => BinOp::Div,
        4 => BinOp::Mod,
        5 => BinOp::Shl,
        6 => BinOp::Shr,
        7 => BinOp::Sar,
        8 => BinOp::Lt,
        9 => BinOp::Le,
        10 => BinOp::Gt,
        11 => BinOp::Ge,
        12 => BinOp::Eq,
        13 => BinOp::Ne,
        14 => BinOp::EqStrict,
        15 => BinOp::NeStrict,
        16 => BinOp::BAnd,
        17 => BinOp::BOr,
        18 => BinOp::BXor,
        19 => BinOp::BNand,
        20 => BinOp::BNor,
        21 => BinOp::BXnor,
        22 => BinOp::LogAnd,
        23 => BinOp::LogOr,
        24 => BinOp::EqM,
        25 => BinOp::NeM,
        26 => BinOp::LtM,
        27 => BinOp::LeM,
        28 => BinOp::GtM,
        29 => BinOp::GeM,
        30 => BinOp::Rol,
        31 => BinOp::Ror,
        32 => BinOp::Pow,
        t => return Err(format!("bad BinOp tag {}", t)),
    })
}

pub(super) fn w_unop(w: &mut W, op: UnOp) {
    w.u8(match op {
        UnOp::Not => 0,
        UnOp::BNot => 1,
        UnOp::Neg => 2,
        UnOp::Pos => 3,
        UnOp::RedAnd => 4,
        UnOp::RedOr => 5,
        UnOp::RedXor => 6,
        UnOp::RedNand => 7,
        UnOp::RedNor => 8,
        UnOp::RedXnor => 9,
        UnOp::Abs => 10,
    })
}
pub(super) fn r_unop(r: &mut R) -> Result<UnOp, String> {
    Ok(match r.u8()? {
        0 => UnOp::Not,
        1 => UnOp::BNot,
        2 => UnOp::Neg,
        3 => UnOp::Pos,
        4 => UnOp::RedAnd,
        5 => UnOp::RedOr,
        6 => UnOp::RedXor,
        7 => UnOp::RedNand,
        8 => UnOp::RedNor,
        9 => UnOp::RedXnor,
        10 => UnOp::Abs,
        t => return Err(format!("bad UnOp tag {}", t)),
    })
}
