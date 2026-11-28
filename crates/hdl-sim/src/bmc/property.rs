//! Safety-property extraction for BMC: pulls `assert property` / `assert`
//! syscalls out of SVA processes into [`Property`] records. Pure move from
//! `bmc.rs`.

use super::classify::Class;
use hdl_ir::elab::Flat;
use hdl_ir::{Expr, Stmt, UnOp};

/// A safety property extracted from an `assert property` / `assert` process.
#[derive(Clone, Debug)]
pub struct Property {
    /// Antecedent guards (conjunction), evaluated at the trigger step.
    pub guards: Vec<Expr>,
    /// The checked condition.
    pub check: Expr,
    /// `|=>` adds one step of delay (checked at trigger + delay).
    pub delay: usize,
    /// Human-readable origin for reports.
    pub origin: String,
}

pub(super) fn extract_properties(flat: &Flat, cls: &Class) -> Result<Vec<Property>, String> {
    let mut out = Vec::new();
    for &pi in &cls.sva_procs {
        let p = &flat.procs[pi];
        let mut guards = Vec::new();
        extract_from_stmt(&p.body, &mut guards, 0, &p.origin, &mut out)?;
    }
    Ok(out)
}

fn extract_from_stmt(
    s: &Stmt,
    guards: &mut Vec<Expr>,
    delay: usize,
    origin: &str,
    out: &mut Vec<Property>,
) -> Result<(), String> {
    match s {
        Stmt::Spanned { inner, .. } => extract_from_stmt(inner, guards, delay, origin, out),
        Stmt::SysCall { name, args, .. } if name == "sva_assert" || name == "assert" => {
            let Some(cond) = args.first() else {
                return Err("prove: malformed assert (no condition)".to_string());
            };
            out.push(Property {
                guards: guards.clone(),
                check: cond.clone(),
                delay,
                origin: origin.to_string(),
            });
            Ok(())
        }
        Stmt::SysCall { name, .. } => Err(format!(
            "prove: system task '${}' inside an assert process is not supported",
            name
        )),
        Stmt::If { cond, then, els } => {
            guards.push(cond.clone());
            extract_from_stmt(then, guards, delay, origin, out)?;
            guards.pop();
            if let Some(e) = els {
                guards.push(Expr::Un {
                    op: UnOp::Not,
                    e: Box::new(cond.clone()),
                });
                extract_from_stmt(e, guards, delay, origin, out)?;
                guards.pop();
            }
            Ok(())
        }
        Stmt::Block(ss) => {
            for s in ss {
                extract_from_stmt(s, guards, delay, origin, out)?;
            }
            Ok(())
        }
        Stmt::Wait { body, .. } => extract_from_stmt(body, guards, delay + 1, origin, out),
        Stmt::WaitUntil { .. } => Ok(()),
        // Other statements (assignments etc.) belong to the enclosing process,
        // which is still executed normally — only the asserts are extracted.
        _ => Ok(()),
    }
}
