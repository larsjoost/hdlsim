//! Shared scalar helpers: syntax-kind predicates, operator mapping tables, literal/time parsing, did-you-mean, and edge extraction.
//!
//! Extracted verbatim from vhdl_lower.rs (pure move, no logic changes).

use super::*;

pub(crate) fn real_children(n: &SyntaxNode) -> Vec<SyntaxNode> {
    n.children()
        .filter(|c| !matches!(c.kind(), SyntaxKind::Whitespace | SyntaxKind::Comment))
        .collect()
}

pub(super) fn is_expr_node(k: SyntaxKind) -> bool {
    matches!(
        k,
        SyntaxKind::Literal
            | SyntaxKind::Name
            | SyntaxKind::ParenExpr
            | SyntaxKind::BinExpr
            | SyntaxKind::UnaryExpr
            | SyntaxKind::Call
            | SyntaxKind::Concat
            | SyntaxKind::Aggregate
            | SyntaxKind::PartSelect
            | SyntaxKind::Index
            | SyntaxKind::Attr
            | SyntaxKind::Qualified
            | SyntaxKind::CondExpr
            | SyntaxKind::Others
    )
}

/// True for node kinds produced by `parse_decl` — used to tell a generate or
/// block declarative item from a concurrent statement when no `begin`
/// keyword separates them.
pub(super) fn is_decl_kind(k: SyntaxKind) -> bool {
    matches!(
        k,
        SyntaxKind::SignalDecl
            | SyntaxKind::ConstantDecl
            | SyntaxKind::VariableDecl
            | SyntaxKind::ComponentDecl
            | SyntaxKind::TypeDecl
            | SyntaxKind::SubtypeDecl
            | SyntaxKind::Function
            | SyntaxKind::AliasDecl
            | SyntaxKind::AttrDecl
            | SyntaxKind::AttrSpec
            | SyntaxKind::FileDecl
            | SyntaxKind::UseClause
            | SyntaxKind::ForSpec
            | SyntaxKind::PackageInst
            | SyntaxKind::PortClause
            | SyntaxKind::GenericClause
            | SyntaxKind::DisconnectSpec
    )
}

pub(super) fn is_stmt_node(k: SyntaxKind) -> bool {
    matches!(
        k,
        SyntaxKind::SeqAssign
            | SyntaxKind::VarAssign
            | SyntaxKind::If
            | SyntaxKind::Case
            | SyntaxKind::For
            | SyntaxKind::LoopStmt
            | SyntaxKind::Wait
            | SyntaxKind::NullStmt
            | SyntaxKind::AssertStmt
            | SyntaxKind::ExitStmt
            | SyntaxKind::CallStmt
            | SyntaxKind::ReturnStmt
            | SyntaxKind::Block
    )
}

/// Kinds collected as a subprogram's declarative items, in textual order
/// (each may reference the ones before it: params, types, constants, locals).
pub(super) fn is_decl_item(k: SyntaxKind) -> bool {
    matches!(
        k,
        SyntaxKind::PortDecl
            | SyntaxKind::VariableDecl
            | SyntaxKind::ConstantDecl
            | SyntaxKind::AliasDecl
            | SyntaxKind::AttrDecl
            | SyntaxKind::TypeDecl
            | SyntaxKind::SubtypeDecl
            | SyntaxKind::FileDecl
    )
}

pub(super) fn bits_for(n: usize) -> usize {
    (usize::BITS - (n.max(1) - 1).leading_zeros()).max(1) as usize
}

/// Case-insensitive "did you mean" suggestion over known unit names: prefers
/// prefix overlap (`edge_detector` → `edge_detector_entity`), then a small
/// edit distance. Exact (case-insensitive) matches are never suggested.
pub(super) fn did_you_mean<'c>(
    name: &str,
    candidates: impl Iterator<Item = &'c String>,
) -> Option<&'c str> {
    if name.is_empty() {
        return None;
    }
    let n = name.to_ascii_lowercase();
    // Acceptance threshold (as below); the edit distance is always >= the
    // length difference, so candidates longer/shorter than that can be
    // skipped without running the O(n*m) Levenshtein — keeps the suggestion
    // O(candidates) instead of O(candidates * n * m) on designs with many units.
    let max_d = (n.len() / 3).max(3);
    let mut best: Option<(usize, &str)> = None;
    for c in candidates {
        let cl = c.to_ascii_lowercase();
        if cl == n {
            continue;
        }
        let prefix = cl.starts_with(&n) || n.starts_with(&cl);
        if !prefix && cl.len().abs_diff(n.len()) > max_d {
            continue;
        }
        let d = if prefix {
            cl.len().abs_diff(n.len())
        } else {
            levenshtein_bounded(&n, &cl, max_d)
        };
        if (prefix || d <= max_d) && best.is_none_or(|(bd, _)| d < bd) {
            best = Some((d, c.as_str()));
        }
    }
    best.map(|(_, c)| c)
}

/// VHDL time unit → femtoseconds.
/// Collect the expressions of all `return` statements in a lowered body.
pub(super) fn collect_return_exprs(s: &Stmt, out: &mut Vec<Expr>) {
    match s {
        Stmt::Return(e) => out.push((**e).clone()),
        // `return e` may also lower to a plain assign to the function-name
        // slot (slot 0).
        Stmt::Assign {
            lhs: Expr::Sig(0),
            rhs,
            ..
        } => out.push(rhs.clone()),
        Stmt::Block(ss) => {
            for s in ss {
                collect_return_exprs(s, out);
            }
        }
        Stmt::If { then, els, .. } => {
            collect_return_exprs(then, out);
            if let Some(e) = els {
                collect_return_exprs(e, out);
            }
        }
        Stmt::Case { items, default, .. } => {
            for (_, st) in items {
                collect_return_exprs(st, out);
            }
            if let Some(d) = default {
                collect_return_exprs(d, out);
            }
        }
        Stmt::For { body, .. } | Stmt::Repeat { body, .. } | Stmt::Delay { stmt: body, .. } => {
            collect_return_exprs(body, out)
        }
        Stmt::Spanned { inner, .. } => collect_return_exprs(inner, out),
        _ => {}
    }
}

/// Real-valued conversion (`to_real(x)`, `to_ufixed(x, ...)`, `to_float(x)`,
/// 3-arg `resize`): lowered as `x * 1.0`, which the simulator evaluates as a
/// real-typed (f64) expression. Fixed/float bounds are not modeled.
pub(super) fn as_real(e: Expr) -> Expr {
    Expr::Bin {
        op: BinOp::Mul,
        l: Box::new(e),
        r: Box::new(Expr::Real(1.0)),
    }
}

pub(super) fn time_unit_fs(unit: &str) -> u64 {
    match unit {
        "fs" => 1,
        "ps" => 1_000,
        "ns" => 1_000_000,
        "us" | "µs" => 1_000_000_000,
        "ms" => 1_000_000_000_000,
        "sec" | "s" => 1_000_000_000_000_000,
        "min" => 60_000_000_000_000_000,
        "hr" => 3_600_000_000_000_000_000,
        "um" => 1_000, // micrometre treated as ps-scale placeholder
        "hz" => 1,
        "khz" => 1_000,
        "mhz" => 1_000_000,
        "ghz" => 1_000_000_000,
        _ => 1_000_000,
    }
}

pub(super) fn vhdl_binop(op: &str) -> Option<BinOp> {
    Some(match op {
        "+" => BinOp::Add,
        "-" => BinOp::Sub,
        "*" => BinOp::Mul,
        "/" => BinOp::Div,
        "mod" | "rem" => BinOp::Mod,
        "**" => BinOp::Pow,
        // VHDL equality is a predefined value comparison (LRM 7.2), not the
        // X-propagating loose ops: `'U' = '0'` is FALSE and `'U' /= '0'` is
        // TRUE, so a guarded wait whose condition reads `sig /= '0'` while
        // the signal is still uninitialized must be taken. The lowering
        // keeps the strict ops for scalar operands; wide (composite) ones
        // are rewritten back to the loose ops in `mark_signed_compare`,
        // where an X condition takes no action.
        "=" => BinOp::EqStrict,
        "/=" => BinOp::NeStrict,
        "?=" => BinOp::EqM,
        "?/=" => BinOp::NeM,
        "<" => BinOp::Lt,
        "<=" => BinOp::Le,
        ">" => BinOp::Gt,
        ">=" => BinOp::Ge,
        "?<" => BinOp::LtM,
        "?<=" => BinOp::LeM,
        "?>" => BinOp::GtM,
        "?>=" => BinOp::GeM,
        "and" => BinOp::BAnd,
        "or" => BinOp::BOr,
        "xor" => BinOp::BXor,
        "nand" => BinOp::BNand,
        "nor" => BinOp::BNor,
        "xnor" => BinOp::BXnor,
        "sll" | "sla" => BinOp::Shl,
        "srl" => BinOp::Shr,
        "sra" => BinOp::Sar,
        "rol" => BinOp::Rol,
        "ror" => BinOp::Ror,
        _ => return None,
    })
}

pub(super) fn vhdl_unop(op: &str) -> Option<UnOp> {
    Some(match op {
        "-" => UnOp::Neg,
        "+" => UnOp::Pos,
        "not" => UnOp::BNot,
        "abs" => UnOp::Abs,
        // `??x` (VHDL-2008 condition operator) lexes as two `?` puncts; the
        // lowerer sees the first one — identity to boolean.
        "?" => UnOp::Pos,
        // Reduction operators: `and x`, `or x`, ... (unary form).
        "and" => UnOp::RedAnd,
        "or" => UnOp::RedOr,
        "xor" => UnOp::RedXor,
        "nand" => UnOp::RedNand,
        "nor" => UnOp::RedNor,
        "xnor" => UnOp::RedXnor,
        _ => return None,
    })
}

/// Extract edge sensitivities from a wait-until condition: rising_edge(s) →
/// Pos edge on s; anything else → Any edge on every signal in the condition.
pub(super) fn edges_from_cond(c: &Expr, lx: &Lower) -> Option<Vec<Edge>> {
    match c {
        Expr::Rising(inner) | Expr::Falling(inner) => {
            if let Expr::Sig(s) = &**inner {
                return Some(vec![Edge {
                    sig: *s,
                    kind: if matches!(c, Expr::Rising(_)) {
                        EdgeKind::Pos
                    } else {
                        EdgeKind::Neg
                    },
                }]);
            }
            None
        }
        _ => {
            let mut sigs = Vec::new();
            c.collect_sigs(&mut sigs);
            let _ = lx;
            if sigs.is_empty() {
                None
            } else {
                Some(
                    sigs.iter()
                        .map(|s| Edge {
                            sig: *s,
                            kind: EdgeKind::Any,
                        })
                        .collect(),
                )
            }
        }
    }
}

/// Build a Value from a u128 with the minimal width that holds it.
pub(super) fn value_from_u128(width: usize, v: u128) -> Value {
    let mut bits = vec![V0; width.max(1)];
    for (i, b) in bits.iter_mut().enumerate().take(width.min(128)) {
        *b = if (v >> i) & 1 == 1 { V1 } else { V0 };
    }
    Value { bits, real: false }
}
